//! Minimal logger writing to stderr and, once opened, to a log file.
//!
//! When the dashboard is started from a shortcut or at sign-in there is no
//! console to read, so the file is the only record of why something failed.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

static SINK: Mutex<Option<File>> = Mutex::new(None);

/// A log larger than this is moved aside at startup rather than grown forever.
const ROTATE_BYTES: u64 = 2 * 1024 * 1024;

/// Direct subsequent log lines to a file as well as stderr.
pub fn open_file(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(path).map(|m| m.len() > ROTATE_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        if let Ok(mut sink) = SINK.lock() {
            *sink = Some(file);
        }
    }
}

pub fn write(level: &str, message: &str) {
    let stamp = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
    let line = format!("{stamp} {level:<5} {message}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Ok(mut sink) = SINK.lock() {
        if let Some(file) = sink.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::write("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::write("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::write("ERROR", &format!($($arg)*)) };
}
