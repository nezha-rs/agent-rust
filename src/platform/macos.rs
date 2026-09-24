//! macOS specialist implementation.

use anyhow::Result;
use std::{
    io,
    path::{Path, PathBuf},
};
use sysinfo::{Disks, System};
use tokio::process::Command;

#[path = "macos/transfer.rs"]
mod transfer;
pub(crate) use transfer::TransferTarget;
pub(crate) type TransferTemp = tempfile::NamedTempFile;

pub(crate) fn terminal_command() -> io::Result<portable_pty::CommandBuilder> {
    Ok(portable_pty::CommandBuilder::new_default_prog())
}

pub(crate) fn http_client_builder(
    builder: reqwest::ClientBuilder,
    _servers: &[String],
) -> io::Result<reqwest::ClientBuilder> {
    Ok(builder)
}

pub(crate) async fn resolve_first_ip(
    host: &str,
    _servers: &[String],
) -> io::Result<std::net::IpAddr> {
    if let Ok(address) = host.parse() {
        return Ok(address);
    }
    tokio::net::lookup_host((host, 0))
        .await?
        .next()
        .map(|address| address.ip())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "DNS returned no addresses"))
}

pub(crate) async fn connect_tcp_host(
    host: &str,
    port: u16,
    _servers: &[String],
) -> io::Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect((host, port)).await
}

pub(crate) fn temperatures() -> Vec<crate::proto::StateSensorTemperature> {
    Vec::new()
}

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

pub(crate) fn ping_command(host: &str) -> Command {
    let mut command = Command::new("ping");
    command.args(["-c", "5", "-W", "3", host]);
    command
}

pub(crate) fn ping_average(output: &str) -> Option<f32> {
    output
        .lines()
        .find(|line| line.contains("min/avg/max"))?
        .split('=')
        .nth(1)?
        .trim()
        .split('/')
        .nth(1)?
        .parse()
        .ok()
}

pub(crate) async fn icmp_ping(target: std::net::IpAddr) -> Result<Option<f32>, String> {
    use std::time::Duration;
    let mut ping = ping_command(&target.to_string());
    ping.kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(20), ping.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(Some(
            ping_average(&String::from_utf8_lossy(&output.stdout)).unwrap_or(0.0),
        )),
        Ok(Ok(output)) if output.stderr.is_empty() => Ok(None),
        Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).to_string()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("ping timed out".into()),
    }
}

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
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn open_legacy_download_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
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
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

pub(crate) fn apply_transfer_mode(file: &std::fs::File, mode: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let bits = if mode.is_empty() {
        0o644
    } else {
        u32::from_str_radix(mode, 8)? & 0o777
    };
    file.set_permissions(std::fs::Permissions::from_mode(bits))?;
    Ok(())
}

pub(crate) fn sync_transfer_parent(path: &Path) -> io::Result<()> {
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?
    .sync_all()
}

pub(crate) fn update_executable_name() -> &'static str {
    "nezha-agent-rust"
}

pub(crate) fn prepare_update_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

pub(crate) fn entry_mode(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bits = meta.permissions().mode() & 0o777;
    if bits == 0 {
        "0".into()
    } else {
        format!("0{bits:o}")
    }
}

pub(crate) fn virtualization() -> String {
    String::new()
}

pub(crate) fn platform_info() -> (String, String) {
    (
        "macos".into(),
        System::long_os_version().unwrap_or_default(),
    )
}

pub(crate) fn swap_used(system: &System) -> u64 {
    system.used_swap()
}

pub(crate) fn connection_counts() -> (u64, u64) {
    (0, 0)
}
