//! Finding, recording and stopping a running dashboard.
//!
//! A running server is recognised by its /api/health reply, never by the
//! port alone, so an unrelated program on the same port is never mistaken
//! for it or stopped. Stopping prefers the graceful, token-protected API; a
//! forced kill is used only on a process whose executable is this program.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::web::{APP_ID, SHUTDOWN_HEADER};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub url: String,
    pub token: String,
    pub started_at: String,
    pub exe: String,
    pub version: String,
}

#[derive(Debug, PartialEq)]
pub enum Probe {
    Ours { pid: u32, version: String },
    Other,
    Nothing,
}

/// Random per-run token for the shutdown endpoint.
pub fn new_token() -> String {
    let mut bytes = [0u8; 24];
    if getrandom::fill(&mut bytes).is_err() {
        // Without an entropy source the endpoint stays closed: no request
        // can match a token nobody can know.
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn write(info: &InstanceInfo) -> std::io::Result<()> {
    let path = paths::instance_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(info).unwrap_or_default();
    std::fs::write(&path, body)?;
    restrict_to_owner(&path);
    Ok(())
}

pub fn read() -> Option<InstanceInfo> {
    let text = std::fs::read_to_string(paths::instance_file()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the record, but only if it still describes this process: a
/// second instance may have replaced it.
pub fn remove_if_ours(pid: u32) {
    if read().is_some_and(|info| info.pid == pid) {
        let _ = std::fs::remove_file(paths::instance_file());
    }
}

#[cfg(unix)]
pub fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// On Windows the per-user profile directories are already private.
#[cfg(not(unix))]
pub fn restrict_to_owner(_path: &Path) {}

/// What, if anything, is serving `host:port`.
pub fn probe(host: &str, port: u16) -> Probe {
    let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) else {
        return Probe::Nothing;
    };
    let reachable = addrs
        .into_iter()
        .any(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(700)).is_ok());
    if !reachable {
        return Probe::Nothing;
    }
    let Ok(response) =
        crate::httpmini::request(host, port, "GET", "/api/health", &[], Duration::from_secs(3))
    else {
        return Probe::Other;
    };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&response.body) else {
        return Probe::Other;
    };
    if response.status == 200 && body["app"] == APP_ID {
        Probe::Ours {
            pid: body["pid"].as_u64().unwrap_or(0) as u32,
            version: body["version"].as_str().unwrap_or("?").to_string(),
        }
    } else {
        Probe::Other
    }
}

/// Wait for a server to appear, for commands that open the dashboard while
/// a server may still be starting.
pub fn wait_for_ours(host: &str, port: u16, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        if matches!(probe(host, port), Probe::Ours { .. }) {
            return true;
        }
        if started.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn wait_until_gone(host: &str, port: u16, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !matches!(probe(host, port), Probe::Ours { .. }) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Stop the dashboard serving `host:port`. Returns a description of what
/// happened, or why nothing was stopped.
pub fn stop(host: &str, port: u16) -> Result<String, String> {
    let pid = match probe(host, port) {
        Probe::Nothing => return Err(format!("Nothing is listening on {host}:{port}.")),
        Probe::Other => {
            return Err(format!(
                "Port {port} is held by another program, not PLH Rack Monitor; it was left alone."
            ));
        }
        Probe::Ours { pid, .. } => pid,
    };

    // Graceful: the token proves the caller can read this user's files.
    if let Some(info) = read().filter(|i| i.port == port && !i.token.is_empty()) {
        let accepted = crate::httpmini::request(
            host,
            port,
            "POST",
            "/api/shutdown",
            &[(SHUTDOWN_HEADER, info.token.as_str())],
            Duration::from_secs(3),
        )
        .map(|r| r.status == 202)
        .unwrap_or(false);
        if accepted && wait_until_gone(host, port, Duration::from_secs(6)) {
            return Ok(format!("Stopped PLH Rack Monitor (PID {pid}) on port {port}."));
        }
    }

    // Forced, and only against a process that is this program.
    kill_ours(pid)?;
    if wait_until_gone(host, port, Duration::from_secs(4)) {
        Ok(format!("Stopped PLH Rack Monitor (PID {pid}) on port {port} (forced)."))
    } else {
        Err(format!("PID {pid} did not stop. It may be running with higher privileges."))
    }
}

fn kill_ours(pid: u32) -> Result<(), String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    if pid == 0 {
        return Err("The running server did not report its PID.".into());
    }
    let target = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    let process = sys.process(target).ok_or_else(|| format!("PID {pid} is not running."))?;
    let name = process.name().to_string_lossy().to_ascii_lowercase();
    if !name.starts_with(paths::EXE_STEM) {
        return Err(format!("PID {pid} is {name}, not PLH Rack Monitor; it was left alone."));
    }
    if process.kill() {
        Ok(())
    } else {
        Err(format!("PID {pid} could not be stopped (Access denied?). Stop it from an elevated shell."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_long() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
    }

    #[test]
    fn closed_port_probes_as_nothing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(probe("127.0.0.1", port), Probe::Nothing);
    }

    #[test]
    fn foreign_server_probes_as_other_and_is_not_stopped() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}");
            }
        });
        assert_eq!(probe("127.0.0.1", port), Probe::Other);
        let refused = stop("127.0.0.1", port).unwrap_err();
        assert!(refused.contains("another program"));
    }
}
