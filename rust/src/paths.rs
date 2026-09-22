//! Where configuration, runtime state and the installed binary live.
//!
//! Every location is per-user, so neither installing nor running the
//! dashboard needs administrator rights on any platform.

use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

pub const APP_NAME: &str = "PLH Rack Monitor";
pub const EXE_STEM: &str = "plh-rack-monitor";

fn project() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", APP_NAME)
}

/// Directory holding config.toml and imported certificates.
/// PLH_CONFIG_DIR overrides it, for portable use or an isolated test run.
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PLH_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    project()
        .map(|p| p.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory for runtime state: the instance record and the log.
/// PLH_DATA_DIR overrides it, for portable use or an isolated test run.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PLH_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    project()
        .map(|p| p.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn instance_file() -> PathBuf {
    data_dir().join("instance.json")
}

pub fn log_file() -> PathBuf {
    data_dir().join("plh-rack-monitor.log")
}

/// A config.toml beside the executable, which makes a copy on removable
/// media self-contained.
pub fn portable_config() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("config.toml");
    candidate.is_file().then_some(candidate)
}

/// The configuration file in effect, by precedence: an explicit path, the
/// PLH_CONFIG variable, a portable file beside the binary, then the per-user
/// configuration directory.
pub fn resolve_config(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var("PLH_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Some(path) = portable_config() {
        return path;
    }
    config_dir().join("config.toml")
}

/// Default per-user installation directory for the binary.
pub fn install_dir() -> PathBuf {
    let base = BaseDirs::new();
    if cfg!(windows) {
        base.map(|b| b.data_local_dir().join("Programs").join(APP_NAME))
            .unwrap_or_else(|| PathBuf::from(APP_NAME))
    } else if cfg!(target_os = "macos") {
        base.map(|b| b.home_dir().join("Applications").join(APP_NAME))
            .unwrap_or_else(|| PathBuf::from(APP_NAME))
    } else {
        base.map(|b| b.home_dir().join(".local").join("bin"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

pub fn exe_name() -> String {
    format!("{EXE_STEM}{}", std::env::consts::EXE_SUFFIX)
}
