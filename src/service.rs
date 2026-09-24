use crate::config::{Args, ServiceAction};
#[cfg(not(any(target_os = "linux", windows)))]
use anyhow::bail;
#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
use std::path::{Path, PathBuf};

#[allow(clippy::needless_return)]
pub fn control(args: &Args, action: ServiceAction, user: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return systemd::control(args, action, user);
    }
    #[cfg(windows)]
    {
        return windows::control(args, action, user);
    }
    #[cfg(target_os = "macos")]
    {
        return launchd::control(args, action, user);
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (args, action, user);
        bail!("service management is currently supported only on Linux, macOS, and Windows")
    }
}

#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
fn absolute_config_path(args: &Args, executable: &Path) -> Result<PathBuf> {
    let path = args
        .config
        .clone()
        .unwrap_or_else(|| executable.with_file_name("config.yml"));
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(any(target_os = "linux", windows, target_os = "macos"))]
fn service_name(executable: &Path, config: &Path) -> Result<String> {
    let name = executable
        .file_name()
        .context("executable has no file name")?
        .to_string_lossy();
    if config == executable.with_file_name("config.yml") {
        return Ok(name.into_owned());
    }
    let digest = md5::compute(config.to_string_lossy().as_bytes());
    let hash = format!("{digest:x}");
    Ok(format!("{name}-{}", &hash[..7]))
}

#[cfg(target_os = "linux")]
include!("platform/linux/service.rs");
#[cfg(windows)]
include!("platform/windows/service.rs");
#[cfg(target_os = "macos")]
include!("platform/macos/service.rs");
