//! PLH Rack Monitor: a local, real-time dashboard for the machine it runs
//! on and any number of Proxmox VE nodes.
//!
//! Release builds on Windows use the GUI subsystem, so starting from the
//! Start Menu or at sign-in shows no console window. When started from a
//! terminal the process attaches to that terminal so output still appears.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod browser;
mod collect;
mod config;
mod history;
mod httpmini;
mod install;
mod instance;
mod logging;
mod model;
mod paths;
mod rates;
mod service;
mod web;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::instance::{InstanceInfo, Probe};

#[derive(Parser)]
#[command(
    name = "plh-rack-monitor",
    version,
    about = "Local real-time dashboard for this machine and its Proxmox VE nodes",
    after_help = "With no command, `run` is assumed: start the dashboard if needed and open it."
)]
struct Cli {
    /// Configuration file [default: per-user configuration directory]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Also read settings from a PLH .env file (the Python edition's format)
    #[arg(long, global = true, value_name = "FILE")]
    env_file: Option<PathBuf>,
    /// Port to serve on, overriding the configuration
    #[arg(long, global = true)]
    port: Option<u16>,
    /// Address to bind, overriding the configuration
    #[arg(long, global = true, value_name = "ADDR")]
    bind: Option<String>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the dashboard if it is not running, then open it
    Run {
        /// Do not open a browser
        #[arg(long)]
        no_browser: bool,
        /// Return immediately and keep the server running detached
        #[arg(long)]
        background: bool,
        /// Add synthetic nodes labelled DEV_MOCK, for layout work
        #[arg(long, hide = true, default_value_t = 0)]
        demo_nodes: usize,
    },
    /// Run the server in the foreground without opening a browser
    Serve {
        #[arg(long, hide = true, default_value_t = 0)]
        demo_nodes: usize,
    },
    /// Open the dashboard of a running server
    Open {
        /// Seconds to wait for a server that is still starting
        #[arg(long, default_value_t = 20)]
        wait: u64,
    },
    /// Stop the running server
    Stop,
    /// Show whether a server is running and how its collectors are doing
    Status,
    /// Install for the current user (no administrator rights needed)
    Install {
        /// Also start the dashboard automatically at sign-in
        #[arg(long)]
        autostart: bool,
        /// Install into this directory instead of the default
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Do not add a Start Menu / application menu entry
        #[arg(long)]
        no_shortcut: bool,
        /// Do not register in Settings > Apps (Windows)
        #[arg(long)]
        no_register: bool,
        /// Do not start the dashboard after installing
        #[arg(long)]
        no_launch: bool,
    },
    /// Remove the installation
    Uninstall {
        /// Also delete the configuration and logs
        #[arg(long)]
        purge: bool,
    },
    /// Inspect or create the configuration
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Convert a PLH .env file (Python edition) into config.toml
    ImportEnv {
        /// Path to the .env file
        path: PathBuf,
        /// Write the result instead of printing it
        #[arg(long)]
        write: bool,
        /// Overwrite an existing config.toml
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the path of the configuration file in use
    Path,
    /// Print the effective configuration (credentials omitted)
    Show,
    /// Write the annotated default configuration
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Validate the configuration and list any problems
    Check,
}

fn main() -> ExitCode {
    #[cfg(windows)]
    attach_parent_console();

    let cli = Cli::parse();
    let config_path = paths::resolve_config(cli.config.as_deref());
    let overrides = config::Overrides { bind_host: cli.bind.clone(), port: cli.port, env_file: cli.env_file.clone() };
    let command = cli.command.unwrap_or(Cmd::Run { no_browser: false, background: false, demo_nodes: 0 });

    let outcome = match command {
        Cmd::Run { no_browser, background, demo_nodes } => {
            if background {
                respawn_detached()
            } else {
                run(&config_path, &overrides, !no_browser, demo_nodes)
            }
        }
        Cmd::Serve { demo_nodes } => run(&config_path, &overrides, false, demo_nodes),
        Cmd::Open { wait } => open(&config_path, &overrides, wait),
        Cmd::Stop => stop(&config_path, &overrides),
        Cmd::Status => status(&config_path, &overrides),
        Cmd::Install { autostart, dir, no_shortcut, no_register, no_launch } => {
            let opts = install::InstallOptions {
                dir,
                autostart,
                shortcut: !no_shortcut,
                register: !no_register,
                launch: !no_launch,
            };
            install::install(&opts).map(print_lines)
        }
        Cmd::Uninstall { purge } => install::uninstall(purge).map(print_lines),
        Cmd::Config { action } => config_command(action, &config_path, &overrides),
        Cmd::ImportEnv { path, write, force } => import_env(&path, &config_path, write, force),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            logging::write("ERROR", &message);
            ExitCode::FAILURE
        }
    }
}

fn print_lines(lines: Vec<String>) {
    for line in lines {
        println!("{line}");
    }
}

// --------------------------------------------------------------------- run

fn run(config_path: &Path, overrides: &config::Overrides, open_browser: bool, demo_nodes: usize) -> Result<(), String> {
    let settings = config::load(config_path, overrides);
    logging::open_file(&paths::log_file());
    for warning in &settings.warnings {
        log_warn!("config: {warning}");
    }

    let host = settings.dial_host();
    let url = settings.url();
    match instance::probe(&host, settings.port) {
        Probe::Ours { pid, version } => {
            println!("PLH Rack Monitor {version} is already running (PID {pid}) at {url}");
            if open_browser {
                let message = browser::open(&url, &settings.browser)?;
                println!("{message}");
            }
            return Ok(());
        }
        Probe::Other => {
            return Err(format!(
                "Port {} is in use by another program. Choose another port with --port, or server.port in {}",
                settings.port,
                settings.config_path.display()
            ));
        }
        Probe::Nothing => {}
    }
    serve(settings, open_browser, demo_nodes)
}

fn serve(settings: config::Settings, open_browser: bool, demo_nodes: usize) -> Result<(), String> {
    #[cfg(feature = "proxmox")]
    collect::proxmox::install_crypto();

    let settings = Arc::new(settings);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("plh-async")
        .enable_all()
        .build()
        .map_err(|e| format!("Cannot start the async runtime: {e}"))?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((settings.bind_host.as_str(), settings.port))
            .await
            .map_err(|e| format!("Cannot listen on {}:{}: {e}", settings.bind_host, settings.port))?;

        let svc = service::Service::new(settings.clone(), demo_nodes);
        svc.start();

        let pid = std::process::id();
        let url = settings.url();
        let token = instance::new_token();
        let info = InstanceInfo {
            pid,
            host: settings.dial_host(),
            port: settings.port,
            url: url.clone(),
            token: token.clone(),
            started_at: model::now_iso(),
            exe: std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        if let Err(e) = instance::write(&info) {
            log_warn!("could not record the running instance: {e}");
        }
        log_info!(
            "PLH Rack Monitor {} serving {url} (PID {pid}, config {})",
            env!("CARGO_PKG_VERSION"),
            settings.config_path.display()
        );

        let stopper = svc.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                log_info!("interrupted");
                stopper.stop();
            }
        });
        #[cfg(unix)]
        {
            let stopper = svc.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{SignalKind, signal};
                if let Ok(mut term) = signal(SignalKind::terminate()) {
                    term.recv().await;
                    log_info!("terminated");
                    stopper.stop();
                }
            });
        }

        if open_browser {
            let browser_cfg = settings.browser.clone();
            if browser_cfg.open_on_run {
                let target = url.clone();
                tokio::task::spawn_blocking(move || match browser::open(&target, &browser_cfg) {
                    Ok(message) => log_info!("{message}"),
                    Err(message) => log_warn!("{message}"),
                });
            }
        }

        let app = web::router(web::AppState::new(svc.clone(), token));
        let graceful = svc.shutdown.clone();
        let server = async move {
            axum::serve(listener, app).with_graceful_shutdown(async move { graceful.wait_forever().await }).await
        };
        // An open event stream can hold a graceful shutdown open; after the
        // signal, three seconds are allowed before the process exits anyway.
        let deadline = svc.shutdown.clone();
        tokio::select! {
            result = server => {
                if let Err(e) = result {
                    log_error!("server error: {e}");
                }
            }
            _ = async move {
                deadline.wait_forever().await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            } => log_warn!("connections still open at shutdown; closing them"),
        }
        instance::remove_if_ours(pid);
        log_info!("stopped");
        Ok(())
    })
}

/// Start a detached copy of this program with the same arguments minus
/// `--background`, then return.
fn respawn_detached() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).filter(|a| a != "--background").collect();
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|e| format!("Could not start in the background: {e}"))?;
    println!("Started in the background (PID {}). Stop it with: plh-rack-monitor stop", child.id());
    Ok(())
}

// ---------------------------------------------------------- other commands

fn open(config_path: &Path, overrides: &config::Overrides, wait: u64) -> Result<(), String> {
    let settings = config::load(config_path, overrides);
    let (host, port, url) = target(&settings);
    if !instance::wait_for_ours(&host, port, Duration::from_secs(wait)) {
        return Err(format!("PLH Rack Monitor is not running on port {port}. Start it with: plh-rack-monitor run"));
    }
    println!("{}", browser::open(&url, &settings.browser)?);
    Ok(())
}

/// Where the running server is: the instance record when present (it may
/// have been started with --port), otherwise the configuration.
fn target(settings: &config::Settings) -> (String, u16, String) {
    match instance::read() {
        Some(info) if matches!(instance::probe(&info.host, info.port), Probe::Ours { .. }) => {
            (info.host, info.port, info.url)
        }
        _ => (settings.dial_host(), settings.port, settings.url()),
    }
}

fn stop(config_path: &Path, overrides: &config::Overrides) -> Result<(), String> {
    let settings = config::load(config_path, overrides);
    let (host, port) = if overrides.port.is_some() {
        (settings.dial_host(), settings.port)
    } else {
        let (h, p, _) = target(&settings);
        (h, p)
    };
    let message = instance::stop(&host, port)?;
    println!("{message}");
    Ok(())
}

fn status(config_path: &Path, overrides: &config::Overrides) -> Result<(), String> {
    let settings = config::load(config_path, overrides);
    let (host, port, url) = target(&settings);
    match instance::probe(&host, port) {
        Probe::Ours { pid, version } => {
            println!("Running: PLH Rack Monitor {version}, PID {pid}, {url}");
            if let Ok(r) = httpmini::request(&host, port, "GET", "/api/health", &[], Duration::from_secs(3)) {
                let health: serde_json::Value = serde_json::from_str(&r.body).unwrap_or_default();
                let service = &health["service"];
                println!(
                    "Service: {} (up {:.0}s)",
                    service["status"].as_str().unwrap_or("?"),
                    service["uptime_seconds"].as_f64().unwrap_or(0.0)
                );
                if let Some(collectors) = service["collectors"].as_object() {
                    for (name, c) in collectors {
                        let state = if c["stale"].as_bool() == Some(true) { "STALE" } else { "ok" };
                        let error = c["last_error"].as_str().map(|e| format!("  last error: {e}")).unwrap_or_default();
                        println!(
                            "  {name:<16} {state:<5} runs {:<6} failures {}{error}",
                            c["runs"].as_u64().unwrap_or(0),
                            c["failures"].as_u64().unwrap_or(0)
                        );
                    }
                }
            }
        }
        Probe::Other => println!("Not running. Port {port} is held by another program."),
        Probe::Nothing => println!("Not running."),
    }
    println!(
        "Config: {} ({})",
        settings.config_path.display(),
        if settings.config_found { "found" } else { "not found; defaults in use" }
    );
    for warning in &settings.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

fn config_command(action: ConfigCmd, config_path: &Path, overrides: &config::Overrides) -> Result<(), String> {
    match action {
        ConfigCmd::Path => {
            let note = if config_path.exists() { "exists" } else { "not created yet; defaults in use" };
            println!("{} ({note})", config_path.display());
            Ok(())
        }
        ConfigCmd::Show => {
            let settings = config::load(config_path, overrides);
            let label = if settings.host.label.trim().is_empty() {
                sysinfo::System::host_name().unwrap_or_else(|| "HOST".into())
            } else {
                settings.host.label.clone()
            };
            let body = settings.public(&label);
            println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            Ok(())
        }
        ConfigCmd::Init { force } => {
            if config_path.exists() && !force {
                return Err(format!("{} already exists (use --force to replace it)", config_path.display()));
            }
            install::write_template(config_path)?;
            println!("Wrote {}", config_path.display());
            Ok(())
        }
        ConfigCmd::Check => {
            let settings = config::load(config_path, overrides);
            println!(
                "{} ({})",
                settings.config_path.display(),
                if settings.config_found { "found" } else { "not found; defaults in use" }
            );
            for node in &settings.nodes {
                let state = match node.unconfigured_reason() {
                    None => "ready".to_string(),
                    Some(reason) => format!("{} - {reason}", node.status_when_unpolled()),
                };
                println!("  {} ({}): {state}", node.name, if node.host.is_empty() { "no host" } else { &node.host });
            }
            if settings.warnings.is_empty() {
                println!("No problems found.");
                Ok(())
            } else {
                for warning in &settings.warnings {
                    println!("  warning: {warning}");
                }
                Err(format!("{} problem(s) found", settings.warnings.len()))
            }
        }
    }
}

fn import_env(env_path: &Path, config_path: &Path, write: bool, force: bool) -> Result<(), String> {
    if !env_path.is_file() {
        return Err(format!("{} not found", env_path.display()));
    }
    let mut file = config::FileConfig::default();
    let mut warnings = Vec::new();
    config::apply_legacy_env(&mut file, env_path, &mut warnings);

    if write {
        if config_path.exists() && !force {
            return Err(format!("{} already exists (use --force to replace it)", config_path.display()));
        }
        let dir = config_path.parent().map(Path::to_path_buf).unwrap_or_default();
        // CA files are copied beside the configuration, so the installed
        // program no longer depends on the old project folder.
        for node in &mut file.nodes {
            let source = PathBuf::from(&node.ca_cert);
            if node.ca_cert.is_empty() || !source.is_file() {
                continue;
            }
            let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or("ca.pem".into());
            let certs = dir.join("certs");
            std::fs::create_dir_all(&certs).map_err(|e| format!("Cannot create {}: {e}", certs.display()))?;
            std::fs::copy(&source, certs.join(&name)).map_err(|e| format!("Cannot copy {}: {e}", source.display()))?;
            node.ca_cert = format!("certs/{name}");
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        std::fs::write(config_path, config::to_toml(&file))
            .map_err(|e| format!("Cannot write {}: {e}", config_path.display()))?;
        instance::restrict_to_owner(config_path);
        println!("Wrote {} with {} Proxmox node(s)", config_path.display(), file.nodes.len());
    } else {
        let mut shown = file.clone();
        for node in &mut shown.nodes {
            if !node.token_secret.is_empty() {
                node.token_secret = "********".into();
            }
        }
        println!("{}", config::to_toml(&shown));
        eprintln!("(Dry run with secrets masked. Add --write to save to {}.)", config_path.display());
    }

    let settings = config::validate(file, config_path.parent().unwrap_or(Path::new(".")), config_path, true, warnings);
    for node in &settings.nodes {
        let state = node.unconfigured_reason().unwrap_or_else(|| "ready".into());
        eprintln!("  {}: {state}", node.name);
    }
    for warning in &settings.warnings {
        eprintln!("  warning: {warning}");
    }
    Ok(())
}

/// Attach to the terminal that started this process, if any, so a GUI
/// subsystem build can still print. Standard handles that were not
/// redirected are pointed at the console.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let redirected = !stdout.is_null() && stdout != INVALID_HANDLE_VALUE;
        if redirected || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let console = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if console != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, console);
            if GetStdHandle(STD_ERROR_HANDLE).is_null() {
                SetStdHandle(STD_ERROR_HANDLE, console);
            }
        }
    }
}
