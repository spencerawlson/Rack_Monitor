//! Per-user installation and removal.
//!
//! Nothing here needs administrator rights. The binary is copied to a
//! per-user program directory, a default config.toml is written if none
//! exists, and the platform's menu entry is added. Starting at sign-in is
//! added only when explicitly requested.
//!
//! Everything created is recorded in a manifest, so uninstall removes
//! exactly what install added and nothing else. Configuration is kept on
//! uninstall unless --purge is given.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub dir: Option<PathBuf>,
    pub autostart: bool,
    /// macOS has no menu entry to add; the flag is read on the others.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub shortcut: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub register: bool,
    pub launch: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub exe: String,
    pub dir: String,
    pub config: String,
    pub created: Vec<String>,
    pub autostart: bool,
    pub registered: bool,
}

fn manifest_path() -> PathBuf {
    paths::data_dir().join("install.json")
}

fn read_manifest() -> Option<Manifest> {
    serde_json::from_str(&std::fs::read_to_string(manifest_path()).ok()?).ok()
}

pub fn install(opts: &InstallOptions) -> Result<Vec<String>, String> {
    let mut report = Vec::new();
    let dir = opts.dir.clone().unwrap_or_else(paths::install_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let target = dir.join(paths::exe_name());
    let current = std::env::current_exe().map_err(|e| format!("Cannot locate this program: {e}"))?;

    if !same_file(&current, &target) {
        // A dashboard running from the target locks the file on Windows.
        if let Some(info) = crate::instance::read() {
            if same_file(Path::new(&info.exe), &target) {
                match crate::instance::stop(&info.host, info.port) {
                    Ok(msg) => report.push(format!("{msg} (to replace its binary)")),
                    Err(msg) => report.push(format!("Note: {msg}")),
                }
            }
        }
        copy_with_retry(&current, &target)?;
        make_executable(&target);
        report.push(format!("Installed {}", target.display()));
    } else {
        report.push(format!("Already running from {}", target.display()));
    }

    // A portable config beside the source binary does not travel with it;
    // the installed copy uses the per-user configuration directory.
    let config = paths::config_dir().join("config.toml");
    if config.exists() {
        report.push(format!("Kept existing configuration {}", config.display()));
    } else {
        write_template(&config)?;
        report.push(format!("Wrote default configuration {}", config.display()));
    }

    let mut manifest = Manifest {
        version: env!("CARGO_PKG_VERSION").into(),
        exe: target.display().to_string(),
        dir: dir.display().to_string(),
        config: config.display().to_string(),
        ..Manifest::default()
    };
    platform::integrate(&target, &dir, opts, &mut manifest, &mut report)?;

    std::fs::create_dir_all(paths::data_dir()).ok();
    std::fs::write(manifest_path(), serde_json::to_string_pretty(&manifest).unwrap_or_default())
        .map_err(|e| format!("Cannot record the installation: {e}"))?;

    if opts.launch {
        match std::process::Command::new(&target).arg("run").spawn() {
            Ok(_) => report.push("Started the dashboard.".into()),
            Err(e) => report.push(format!("Could not start the dashboard: {e}")),
        }
    }
    Ok(report)
}

pub fn uninstall(purge: bool) -> Result<Vec<String>, String> {
    let mut report = Vec::new();
    let manifest = read_manifest();
    let exe = manifest
        .as_ref()
        .map(|m| PathBuf::from(&m.exe))
        .unwrap_or_else(|| paths::install_dir().join(paths::exe_name()));
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_else(paths::install_dir);

    if let Some(info) = crate::instance::read() {
        match crate::instance::stop(&info.host, info.port) {
            Ok(msg) => report.push(msg),
            Err(msg) => report.push(format!("Note: {msg}")),
        }
    }

    platform::remove(&exe, manifest.as_ref(), &mut report);

    let current = std::env::current_exe().ok();
    if exe.exists() {
        if current.as_deref().is_some_and(|c| same_file(c, &exe)) {
            platform::delete_after_exit(&exe, &dir);
            report.push(format!("{} will be removed when this program exits", exe.display()));
        } else {
            match std::fs::remove_file(&exe) {
                Ok(()) => {
                    report.push(format!("Removed {}", exe.display()));
                    let _ = std::fs::remove_dir(&dir);
                }
                Err(e) => report.push(format!("Could not remove {}: {e}", exe.display())),
            }
        }
    }

    let _ = std::fs::remove_file(manifest_path());
    if purge {
        for d in [paths::config_dir(), paths::data_dir()] {
            if d.exists() && std::fs::remove_dir_all(&d).is_ok() {
                report.push(format!("Removed {}", d.display()));
            }
        }
    } else {
        report.push(format!(
            "Kept configuration in {} (use --purge to remove it)",
            paths::config_dir().display()
        ));
    }
    Ok(report)
}

/// Write the annotated default configuration, readable only by its owner.
pub fn write_template(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, crate::config::TEMPLATE).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    crate::instance::restrict_to_owner(path);
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn copy_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..10 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err(format!("Cannot copy to {}: {last}", to.display()))
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

// ------------------------------------------------ generated file contents

#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub fn desktop_entry(exe: &Path, args: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Local system and Proxmox dashboard\n\
         Exec=\"{}\" {args}\nTerminal=false\nCategories=System;Monitor;\n",
        paths::APP_NAME,
        exe.display()
    )
}

#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub fn systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription={} server\nAfter=network-online.target\n\n\
         [Service]\nExecStart=\"{}\" serve\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        paths::APP_NAME,
        exe.display()
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launch_agent(exe: &Path) -> String {
    let escaped = exe
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>tech.plh.rackmonitor</string>\n\
         <key>ProgramArguments</key><array><string>{escaped}</string><string>run</string></array>\n\
         <key>RunAtLoad</key><true/>\n\
         </dict></plist>\n"
    )
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
mod platform {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
        RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW,
        RegSetValueExW,
    };

    pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\PLHRackMonitor";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open_key(subkey: &str, access: u32) -> Result<HKEY, String> {
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(subkey);
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                access,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_SUCCESS { Ok(key) } else { Err(format!("registry key {subkey}: error {status}")) }
    }

    pub fn set_string(subkey: &str, name: &str, value: &str) -> Result<(), String> {
        let key = open_key(subkey, KEY_WRITE)?;
        let data = wide(value);
        let status = unsafe {
            RegSetValueExW(key, wide(name).as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
        };
        unsafe { RegCloseKey(key) };
        if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry value {name}: error {status}")) }
    }

    pub fn set_dword(subkey: &str, name: &str, value: u32) -> Result<(), String> {
        let key = open_key(subkey, KEY_WRITE)?;
        let status = unsafe {
            RegSetValueExW(key, wide(name).as_ptr(), 0, REG_DWORD, (&value as *const u32).cast(), 4)
        };
        unsafe { RegCloseKey(key) };
        if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry value {name}: error {status}")) }
    }

    /// Read back a value; used to verify writes in the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_string(subkey: &str, name: &str) -> Option<String> {
        let key = open_key(subkey, KEY_READ).ok()?;
        let mut buffer = vec![0u16; 2048];
        let mut size = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                key,
                std::ptr::null(),
                wide(name).as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        unsafe { RegCloseKey(key) };
        if status != ERROR_SUCCESS {
            return None;
        }
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    pub fn delete_value(subkey: &str, name: &str) -> bool {
        let Ok(key) = open_key(subkey, KEY_WRITE) else { return false };
        let status = unsafe { RegDeleteValueW(key, wide(name).as_ptr()) };
        unsafe { RegCloseKey(key) };
        status == ERROR_SUCCESS
    }

    pub fn delete_tree(subkey: &str) -> bool {
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(subkey).as_ptr()) == ERROR_SUCCESS }
    }

    fn start_menu_shortcut() -> Option<PathBuf> {
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from)?;
        Some(roaming.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{}.lnk", paths::APP_NAME)))
    }

    /// Create a .lnk through the Windows Script Host COM object, which is
    /// present on every supported Windows release.
    pub fn create_shortcut(lnk: &Path, target: &Path, dir: &Path) -> Result<(), String> {
        let q = |p: &Path| p.display().to_string().replace('\'', "''");
        let script = format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.Arguments='run';\
             $s.WorkingDirectory='{}';$s.Description='{}';$s.IconLocation='{},0';$s.Save()",
            q(lnk),
            q(target),
            q(dir),
            paths::APP_NAME,
            q(target)
        );
        crate::collect::powershell(&script, std::time::Duration::from_secs(30))
            .map(|_| ())
            .ok_or_else(|| format!("Could not create the shortcut {}", lnk.display()))
    }

    pub fn integrate(
        target: &Path,
        dir: &Path,
        opts: &InstallOptions,
        manifest: &mut Manifest,
        report: &mut Vec<String>,
    ) -> Result<(), String> {
        let quoted = format!("\"{}\"", target.display());
        if opts.shortcut {
            if let Some(lnk) = start_menu_shortcut() {
                create_shortcut(&lnk, target, dir)?;
                manifest.created.push(lnk.display().to_string());
                report.push(format!("Added Start Menu shortcut {}", lnk.display()));
            }
        }
        if opts.register {
            set_string(UNINSTALL_KEY, "DisplayName", paths::APP_NAME)?;
            set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
            set_string(UNINSTALL_KEY, "Publisher", "PLH")?;
            set_string(UNINSTALL_KEY, "InstallLocation", &dir.display().to_string())?;
            set_string(UNINSTALL_KEY, "DisplayIcon", &format!("{},0", target.display()))?;
            set_string(UNINSTALL_KEY, "UninstallString", &format!("{quoted} uninstall"))?;
            set_dword(UNINSTALL_KEY, "NoModify", 1)?;
            set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
            let kb = std::fs::metadata(target).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
            set_dword(UNINSTALL_KEY, "EstimatedSize", kb)?;
            manifest.registered = true;
            report.push("Registered in Settings > Apps (uninstall from there or with `uninstall`)".into());
        }
        if opts.autostart {
            set_string(RUN_KEY, paths::APP_NAME, &format!("{quoted} run"))?;
            manifest.autostart = true;
            report.push("Starts automatically at sign-in".into());
        }
        Ok(())
    }

    pub fn remove(_exe: &Path, manifest: Option<&Manifest>, report: &mut Vec<String>) {
        if delete_value(RUN_KEY, paths::APP_NAME) {
            report.push("Removed start at sign-in".into());
        }
        if delete_tree(UNINSTALL_KEY) {
            report.push("Removed the Settings > Apps entry".into());
        }
        let mut shortcuts: Vec<PathBuf> = manifest
            .map(|m| m.created.iter().map(PathBuf::from).filter(|p| p.extension().is_some_and(|e| e == "lnk")).collect())
            .unwrap_or_default();
        if let Some(default) = start_menu_shortcut() {
            if !shortcuts.contains(&default) {
                shortcuts.push(default);
            }
        }
        for lnk in shortcuts {
            if std::fs::remove_file(&lnk).is_ok() {
                report.push(format!("Removed {}", lnk.display()));
            }
        }
    }

    /// A running .exe cannot delete itself on Windows, so a hidden helper
    /// removes it once this process has exited.
    pub fn delete_after_exit(exe: &Path, dir: &Path) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let command = format!(
            "ping -n 3 127.0.0.1 >NUL & del /F /Q \"{}\" & rmdir \"{}\"",
            exe.display(),
            dir.display()
        );
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg("/C")
            .raw_arg(&command)
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn();
    }
}

// ------------------------------------------------------------ Linux / BSD

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;
    use std::process::Command;

    fn home() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
    }

    pub fn integrate(
        target: &Path,
        _dir: &Path,
        opts: &InstallOptions,
        manifest: &mut Manifest,
        report: &mut Vec<String>,
    ) -> Result<(), String> {
        let home = home().ok_or("Cannot find the home directory")?;
        if opts.shortcut {
            let path = home.join(".local/share/applications/plh-rack-monitor.desktop");
            write_file(&path, &desktop_entry(target, "run"))?;
            manifest.created.push(path.display().to_string());
            report.push(format!("Added application menu entry {}", path.display()));
        }
        if opts.autostart {
            let unit = home.join(".config/systemd/user/plh-rack-monitor.service");
            write_file(&unit, &systemd_unit(target))?;
            manifest.created.push(unit.display().to_string());
            let enabled = Command::new("systemctl").args(["--user", "daemon-reload"]).status().is_ok_and(|s| s.success())
                && Command::new("systemctl")
                    .args(["--user", "enable", "--now", "plh-rack-monitor.service"])
                    .status()
                    .is_ok_and(|s| s.success());
            report.push(if enabled {
                "Enabled systemd user service plh-rack-monitor (run `loginctl enable-linger` to start before sign-in)".into()
            } else {
                format!("Wrote {}; enable it with: systemctl --user enable --now plh-rack-monitor", unit.display())
            });
            // On a desktop, also open the dashboard at sign-in.
            if std::env::var_os("XDG_CURRENT_DESKTOP").is_some() {
                let auto = home.join(".config/autostart/plh-rack-monitor.desktop");
                write_file(&auto, &desktop_entry(target, "open"))?;
                manifest.created.push(auto.display().to_string());
                report.push(format!("Opens the dashboard at sign-in ({})", auto.display()));
            }
            manifest.autostart = true;
        }
        Ok(())
    }

    fn write_file(path: &Path, body: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, body).map_err(|e| format!("Cannot write {}: {e}", path.display()))
    }

    pub fn remove(_exe: &Path, manifest: Option<&Manifest>, report: &mut Vec<String>) {
        if manifest.is_some_and(|m| m.autostart) {
            let _ = Command::new("systemctl").args(["--user", "disable", "--now", "plh-rack-monitor.service"]).status();
        }
        for path in manifest.map(|m| m.created.clone()).unwrap_or_default() {
            if std::fs::remove_file(&path).is_ok() {
                report.push(format!("Removed {path}"));
            }
        }
    }

    /// Unix allows removing a running binary; the file disappears when the
    /// process exits.
    pub fn delete_after_exit(exe: &Path, _dir: &Path) {
        let _ = std::fs::remove_file(exe);
    }
}

// -------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process::Command;

    fn agent_path() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join("Library/LaunchAgents/tech.plh.rackmonitor.plist"))
    }

    pub fn integrate(
        target: &Path,
        _dir: &Path,
        opts: &InstallOptions,
        manifest: &mut Manifest,
        report: &mut Vec<String>,
    ) -> Result<(), String> {
        if opts.autostart {
            let path = agent_path().ok_or("Cannot find the home directory")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, launch_agent(target)).map_err(|e| e.to_string())?;
            let _ = Command::new("launchctl").arg("load").arg(&path).status();
            manifest.created.push(path.display().to_string());
            manifest.autostart = true;
            report.push(format!("Starts at sign-in ({})", path.display()));
        }
        Ok(())
    }

    pub fn remove(_exe: &Path, manifest: Option<&Manifest>, report: &mut Vec<String>) {
        if let Some(path) = agent_path().filter(|p| p.exists()) {
            let _ = Command::new("launchctl").arg("unload").arg(&path).status();
            if std::fs::remove_file(&path).is_ok() {
                report.push(format!("Removed {}", path.display()));
            }
        }
        let _ = manifest;
    }

    pub fn delete_after_exit(exe: &Path, _dir: &Path) {
        let _ = std::fs::remove_file(exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_the_path() {
        let text = desktop_entry(Path::new("/home/me/.local/bin/plh rack"), "run");
        assert!(text.contains("Exec=\"/home/me/.local/bin/plh rack\" run"));
        assert!(text.starts_with("[Desktop Entry]"));
    }

    #[test]
    fn systemd_unit_serves_without_a_browser() {
        let text = systemd_unit(Path::new("/opt/plh/plh-rack-monitor"));
        assert!(text.contains("ExecStart=\"/opt/plh/plh-rack-monitor\" serve"));
        assert!(text.contains("WantedBy=default.target"));
    }

    #[test]
    fn launch_agent_escapes_xml() {
        let text = launch_agent(Path::new("/Users/a&b/plh"));
        assert!(text.contains("<string>/Users/a&amp;b/plh</string>"));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
    }

    #[cfg(windows)]
    #[test]
    fn registry_round_trip_on_a_scratch_key() {
        // A throwaway key: the real Run and Uninstall keys are not touched.
        let key = r"Software\PLHRackMonitorTest";
        platform::set_string(key, "Value", "C:\\Program Files\\x.exe run").unwrap();
        platform::set_dword(key, "Number", 7).unwrap();
        assert_eq!(platform::get_string(key, "Value").as_deref(), Some("C:\\Program Files\\x.exe run"));
        assert!(platform::delete_value(key, "Value"));
        assert!(platform::get_string(key, "Value").is_none());
        assert!(platform::delete_tree(key));
    }

    #[cfg(windows)]
    #[test]
    fn shortcut_is_created() {
        let dir = std::env::temp_dir().join(format!("plh-lnk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lnk = dir.join("PLH Rack Monitor.lnk");
        let target = std::env::current_exe().unwrap();
        platform::create_shortcut(&lnk, &target, &dir).unwrap();
        assert!(lnk.is_file());
        assert!(std::fs::metadata(&lnk).unwrap().len() > 100);
        let _ = std::fs::remove_dir_all(dir);
    }
}
