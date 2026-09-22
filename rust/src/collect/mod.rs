//! Metric collectors. Each degrades to None rather than guessing.

pub mod cputimes;
pub mod health;
pub mod host;
pub mod platform;
#[cfg(feature = "proxmox")]
pub mod proxmox;
pub mod temperature;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run a program with a time limit and return its stdout when it succeeds.
///
/// Output is read on a separate thread while the process runs, so a command
/// that writes more than a pipe buffer holds cannot deadlock against the wait.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> Option<(i32, String)> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    let text = reader.join().unwrap_or_default();
    Some((status.code().unwrap_or(-1), text))
}

/// Run a PowerShell snippet without a window and return stdout on success.
#[cfg(windows)]
pub fn powershell(script: &str, timeout: Duration) -> Option<String> {
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    match run_with_timeout(command, timeout) {
        Some((0, text)) => Some(text),
        _ => None,
    }
}
