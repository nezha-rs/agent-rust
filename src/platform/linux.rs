//! Linux specialist implementation.

use anyhow::Result;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use sysinfo::{Disks, System};
use tokio::process::Command;

#[path = "linux/filesystem.rs"]
mod filesystem;
#[path = "linux/network.rs"]
mod network;
#[path = "linux/ping.rs"]
mod ping;
#[path = "linux/temperature.rs"]
mod temperature;
#[path = "linux/transfer.rs"]
mod transfer;
#[path = "linux/virtualization.rs"]
mod virtualization_probe;

pub(crate) use filesystem::{
    anchor_directory, delete_tree_at, entry_mode, final_target_is_directory, open_regular_at,
    open_write_temp, path_component, unlink_at, validate_final_target,
};
pub(crate) use network::{connect_tcp_host, http_client_builder, resolve_first_ip};
pub(crate) use ping::icmp_ping;
pub(crate) use temperature::temperatures;
pub(crate) use transfer::{TransferTarget, TransferTemp};
pub(crate) use virtualization_probe::virtualization;

pub(crate) async fn wait_terminate() -> Result<()> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    signal.recv().await;
    Ok(())
}

pub(crate) struct ProcessGuard(Option<u32>);

pub(crate) fn new_process_guard() -> io::Result<ProcessGuard> {
    Ok(ProcessGuard(None))
}

pub(crate) fn configure_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

pub(crate) fn attach_process_guard(guard: &mut ProcessGuard, pid: Option<u32>) -> io::Result<()> {
    guard.0 = pid;
    Ok(())
}

impl ProcessGuard {
    pub(crate) fn terminate(&self) {
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) fn shell_command(text: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", text]);
    command
}

pub(crate) fn terminal_command() -> io::Result<portable_pty::CommandBuilder> {
    let paths = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let shell = select_terminal_shell(&paths)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no usable terminal shell"))?;
    let mut command = portable_pty::CommandBuilder::new(shell);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "Nezha");
    Ok(command)
}

fn select_terminal_shell(paths: &[PathBuf]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for shell in ["zsh", "fish", "bash", "sh"] {
        for directory in paths {
            let candidate = directory.join(shell);
            if let Ok(metadata) = fs::metadata(&candidate) {
                if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[allow(clippy::unnecessary_cast)]
pub(crate) fn filesystem_usage(path: &Path) -> Option<(u64, u64)> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize as u64;
    Some((
        stat.f_blocks as u64 * block_size,
        (stat.f_blocks as u64).saturating_sub(stat.f_bfree as u64) * block_size,
    ))
}

pub(crate) fn allowlisted_disk_usage(_disks: &Disks, paths: &[String]) -> (u64, u64) {
    paths
        .iter()
        .filter_map(|path| filesystem_usage(Path::new(path)))
        .fold((0, 0), |(total, used), (size, consumed)| {
            (total + size, used + consumed)
        })
}

pub(crate) fn user_home() -> Option<PathBuf> {
    use std::{
        ffi::{CStr, OsString},
        os::unix::ffi::OsStringExt,
    };
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let mut user = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0_u8; 4096];
    let mut found = std::ptr::null_mut();
    if unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            user.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    } != 0
        || found.is_null()
    {
        return None;
    }
    let user = unsafe { user.assume_init() };
    let home = unsafe { CStr::from_ptr(user.pw_dir) }.to_bytes().to_vec();
    Some(PathBuf::from(OsString::from_vec(home)))
}

pub(crate) fn open_legacy_download_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

pub(crate) fn terminate_pty_child(child: &mut dyn portable_pty::Child) {
    if let Some(group) = child.process_id().and_then(|pid| i32::try_from(pid).ok()) {
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn secure_config_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

pub(crate) fn apply_transfer_mode(file: &fs::File, mode: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let bits = if mode.is_empty() {
        0o644
    } else {
        u32::from_str_radix(mode, 8)? & 0o777
    };
    file.set_permissions(fs::Permissions::from_mode(bits))?;
    Ok(())
}

pub(crate) fn update_executable_name() -> &'static str {
    "nezha-agent-rust"
}

pub(crate) fn prepare_update_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

pub(crate) fn platform_info() -> (String, String) {
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut platform = os_release
        .lines()
        .find_map(|line| line.strip_prefix("ID="))
        .unwrap_or(std::env::consts::OS)
        .trim_matches('"')
        .to_string();
    if platform == "amzn" {
        platform = "amazon".into();
    }
    let version = os_release
        .lines()
        .find_map(|line| line.strip_prefix("VERSION_ID="))
        .unwrap_or("")
        .trim_matches('"')
        .to_string();
    (platform, version)
}

pub(crate) fn swap_used(system: &System) -> u64 {
    system.used_swap()
}

pub(crate) fn connection_counts() -> (u64, u64) {
    (
        count_connections("/proc/net/tcp") + count_connections("/proc/net/tcp6"),
        count_connections("/proc/net/udp") + count_connections("/proc/net/udp6"),
    )
}

fn count_connections(path: &str) -> u64 {
    fs::read_to_string(path)
        .map(|text| text.lines().skip(1).count() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    use std::{fs::File, os::unix::fs::PermissionsExt};

    #[test]
    fn terminal_prefers_upstream_shell_order_and_environment() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path();
        for shell in ["sh", "bash", "fish", "zsh"] {
            let path = directory.join(shell);
            File::create(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            select_terminal_shell(&[directory.to_path_buf()]),
            Some(directory.join("zsh"))
        );
        let command = terminal_command().unwrap();
        assert_eq!(command.get_env("TERM").unwrap(), "xterm-256color");
        assert_eq!(command.get_env("COLORTERM").unwrap(), "truecolor");
        assert_eq!(command.get_env("TERM_PROGRAM").unwrap(), "Nezha");
    }
}
