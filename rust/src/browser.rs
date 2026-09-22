//! Opening the dashboard in a browser, on whatever the machine has.
//!
//! Chromium-family browsers (Chrome, Edge, Chromium) can open an "app"
//! window with no address bar or tabs, which suits a small panel. Anything
//! else falls back to the system default browser.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::BrowserSection;

/// Open `url` as configured. Returns a description of what was launched.
pub fn open(url: &str, cfg: &BrowserSection) -> Result<String, String> {
    if cfg.kind == "none" {
        return Ok("Browser not opened (browser.kind = \"none\").".into());
    }
    let order: &[&str] = match cfg.kind.as_str() {
        "auto" => &["chrome", "edge", "chromium"],
        "default" => &[],
        other => std::slice::from_ref(match other {
            "chrome" => &"chrome",
            "edge" => &"edge",
            "chromium" => &"chromium",
            _ => &"firefox",
        }),
    };
    for kind in order {
        if let Some(exe) = locate(kind) {
            let args = arguments(kind, url, cfg);
            return spawn(&exe, &args).map(|_| format!("Opened {} ({kind})", exe.display()));
        }
    }
    if cfg.kind != "auto" && cfg.kind != "default" {
        crate::log_warn!("browser.kind = {:?} not found; using the default browser", cfg.kind);
    }
    open_default(url)
}

/// Command-line arguments for a browser of the given family.
pub fn arguments(kind: &str, url: &str, cfg: &BrowserSection) -> Vec<String> {
    if kind == "firefox" {
        let mut args = vec!["--new-window".to_string(), url.to_string()];
        if cfg.fullscreen {
            args.insert(0, "--kiosk".into());
        }
        return args;
    }
    let mut args = Vec::new();
    if cfg.app_window {
        args.push(format!("--app={url}"));
        if cfg.window_width > 0 && cfg.window_height > 0 {
            args.push(format!("--window-size={},{}", cfg.window_width, cfg.window_height));
        }
    } else {
        args.push("--new-window".into());
        args.push(url.to_string());
    }
    if cfg.fullscreen {
        args.push("--start-fullscreen".into());
    }
    args
}

fn spawn(exe: &PathBuf, args: &[String]) -> Result<(), String> {
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {}: {e}", exe.display()))
}

fn open_default(url: &str) -> Result<String, String> {
    let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
        ("rundll32.exe", vec!["url.dll,FileProtocolHandler", url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| format!("Opened {url} in the default browser"))
        .map_err(|e| format!("Could not open a browser ({program}: {e}); visit {url} manually"))
}

/// Find a browser executable of the given family.
pub fn locate(kind: &str) -> Option<PathBuf> {
    candidates(kind).into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn candidates(kind: &str) -> Vec<PathBuf> {
    let env = |name: &str| std::env::var(name).ok().map(PathBuf::from);
    let program_files = env("ProgramFiles");
    let program_files_x86 = env("ProgramFiles(x86)");
    let local = env("LOCALAPPDATA");
    let join = |base: &Option<PathBuf>, rest: &str| base.as_ref().map(|b| b.join(rest));
    let list = match kind {
        "chrome" => vec![
            join(&program_files, r"Google\Chrome\Application\chrome.exe"),
            join(&program_files_x86, r"Google\Chrome\Application\chrome.exe"),
            join(&local, r"Google\Chrome\Application\chrome.exe"),
        ],
        "edge" => vec![
            join(&program_files_x86, r"Microsoft\Edge\Application\msedge.exe"),
            join(&program_files, r"Microsoft\Edge\Application\msedge.exe"),
        ],
        "chromium" => vec![join(&local, r"Chromium\Application\chrome.exe")],
        "firefox" => vec![
            join(&program_files, r"Mozilla Firefox\firefox.exe"),
            join(&program_files_x86, r"Mozilla Firefox\firefox.exe"),
        ],
        _ => vec![],
    };
    list.into_iter().flatten().collect()
}

#[cfg(target_os = "macos")]
fn candidates(kind: &str) -> Vec<PathBuf> {
    let app = match kind {
        "chrome" => "Google Chrome.app/Contents/MacOS/Google Chrome",
        "edge" => "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "chromium" => "Chromium.app/Contents/MacOS/Chromium",
        "firefox" => "Firefox.app/Contents/MacOS/firefox",
        _ => return vec![],
    };
    let mut list = vec![PathBuf::from("/Applications").join(app)];
    if let Some(home) = directories::BaseDirs::new() {
        list.push(home.home_dir().join("Applications").join(app));
    }
    list
}

#[cfg(not(any(windows, target_os = "macos")))]
fn candidates(kind: &str) -> Vec<PathBuf> {
    let names: &[&str] = match kind {
        "chrome" => &["google-chrome", "google-chrome-stable"],
        "edge" => &["microsoft-edge", "microsoft-edge-stable"],
        "chromium" => &["chromium", "chromium-browser"],
        "firefox" => &["firefox"],
        _ => &[],
    };
    names.iter().filter_map(|n| which(n)).collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BrowserSection {
        BrowserSection::default()
    }

    #[test]
    fn app_window_arguments() {
        let mut c = cfg();
        c.window_width = 1424;
        c.window_height = 280;
        c.fullscreen = true;
        let args = arguments("chrome", "http://127.0.0.1:8765/", &c);
        assert_eq!(
            args,
            vec!["--app=http://127.0.0.1:8765/", "--window-size=1424,280", "--start-fullscreen"]
        );
    }

    #[test]
    fn plain_window_arguments() {
        let mut c = cfg();
        c.app_window = false;
        let args = arguments("edge", "http://x/", &c);
        assert_eq!(args, vec!["--new-window", "http://x/"]);
    }

    #[test]
    fn firefox_uses_kiosk_for_fullscreen() {
        let mut c = cfg();
        c.fullscreen = true;
        assert_eq!(arguments("firefox", "http://x/", &c)[0], "--kiosk");
    }

    #[test]
    fn none_opens_nothing() {
        let mut c = cfg();
        c.kind = "none".into();
        assert!(open("http://x/", &c).unwrap().contains("not opened"));
    }
}
