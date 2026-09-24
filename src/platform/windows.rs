//! Windows specialist implementation.

use anyhow::Result;
use std::io;
use std::path::{Path, PathBuf};
use sysinfo::{Disks, System};
use tokio::process::Command;

#[path = "windows/filesystem.rs"]
mod filesystem;
#[path = "windows/process_group.rs"]
mod process_group;
#[path = "windows/temperature.rs"]
mod temperature;
#[path = "windows/transfer.rs"]
mod transfer;

pub(crate) use filesystem::{
    anchor_directory, delete_tree as delete_tree_windows, entry_mode, open_directory, open_regular,
};
pub(crate) use temperature::temperatures;
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

pub(crate) async fn wait_terminate() -> Result<()> {
    std::future::pending().await
}

pub(crate) struct ProcessGuard(Option<process_group::WindowsJob>);

pub(crate) fn new_process_guard() -> io::Result<ProcessGuard> {
    Ok(ProcessGuard(Some(process_group::WindowsJob::new()?)))
}

pub(crate) fn configure_command(_command: &mut Command) {}

pub(crate) fn attach_process_guard(guard: &mut ProcessGuard, pid: Option<u32>) -> io::Result<()> {
    if let Some(pid) = pid {
        guard.0.as_ref().expect("Windows job exists").assign(pid)?;
    }
    Ok(())
}

impl ProcessGuard {
    pub(crate) fn terminate(&self) {
        if let Some(job) = self.0.as_ref() {
            job.terminate();
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) fn shell_command(text: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/C", text]);
    command
}

pub(crate) fn ping_command(host: &str) -> Command {
    let mut command = Command::new("ping");
    command.args(["-n", "5", "-w", "3000", host]);
    command
}

pub(crate) fn ping_average(output: &str) -> Option<f32> {
    output.lines().rev().find_map(|line| {
        let value = line.rsplit('=').next()?.trim();
        let milliseconds = value.strip_suffix("ms")?.trim();
        milliseconds.parse().ok()
    })
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
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut total = 0u64;
    let mut free = 0u64;
    let result =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) };
    (result != 0).then_some((total, total.saturating_sub(free)))
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
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let mut drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            drive.push(path);
            Some(PathBuf::from(drive))
        })
}

pub(crate) fn open_legacy_download_file(path: &Path) -> io::Result<std::fs::File> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "download file type is unsupported on this platform: {}",
                path.display()
            ),
        ));
    }
    std::fs::File::open(path)
}

pub(crate) fn terminate_pty_child(child: &mut dyn portable_pty::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn secure_config_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn apply_transfer_mode(file: &std::fs::File, mode: &str) -> Result<()> {
    let bits = if mode.is_empty() {
        0o644
    } else {
        u32::from_str_radix(mode, 8)? & 0o777
    };
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(bits & 0o222 == 0);
    file.set_permissions(permissions)?;
    Ok(())
}

pub(crate) fn update_executable_name() -> &'static str {
    "nezha-agent-rust.exe"
}

pub(crate) fn prepare_update_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn virtualization() -> String {
    String::new()
}

pub(crate) fn platform_info() -> (String, String) {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
    let product = System::long_os_version().unwrap_or_else(|| "Windows".into());
    let version = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion")
        .ok()
        .and_then(|key| key.get_value("DisplayVersion").ok())
        .unwrap_or_default();
    (format!("Microsoft {product}"), version)
}

pub(crate) fn swap_used(system: &System) -> u64 {
    use windows_sys::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
        PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
    };
    let mut query = std::ptr::null_mut();
    if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
        return system.used_swap();
    }
    let path = "\\Paging File(_Total)\\% Usage\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut counter = std::ptr::null_mut();
    let value = unsafe {
        if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != 0
            || PdhCollectQueryData(query) != 0
        {
            None
        } else {
            let mut formatted = PDH_FMT_COUNTERVALUE::default();
            if PdhGetFormattedCounterValue(
                counter,
                PDH_FMT_DOUBLE,
                std::ptr::null_mut(),
                &mut formatted,
            ) == 0
            {
                Some(formatted.Anonymous.doubleValue)
            } else {
                None
            }
        }
    };
    unsafe { PdhCloseQuery(query) };
    value
        .filter(|percent| percent.is_finite() && *percent >= 0.0)
        .map(|percent| (system.total_swap() as f64 * percent / 100.0) as u64)
        .unwrap_or_else(|| system.used_swap())
}

pub(crate) fn connection_counts() -> (u64, u64) {
    let output = std::process::Command::new("netstat").args(["-an"]).output();
    if let Ok(output) = output {
        let mut tcp = 0;
        let mut udp = 0;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            match line.split_whitespace().next() {
                Some("TCP") => tcp += 1,
                Some("UDP") => udp += 1,
                _ => {}
            }
        }
        return (tcp, udp);
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_allowlist_uses_each_requested_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let per_path = filesystem_usage(first.path()).unwrap();
        assert!(per_path.0 > 0);
        let paths = vec![
            first.path().to_string_lossy().into_owned(),
            second.path().to_string_lossy().into_owned(),
        ];
        let total = allowlisted_disk_usage(&Disks::new(), &paths);
        let second_usage = filesystem_usage(second.path()).unwrap();
        assert_eq!(
            total,
            (per_path.0 + second_usage.0, per_path.1 + second_usage.1)
        );
    }
}
