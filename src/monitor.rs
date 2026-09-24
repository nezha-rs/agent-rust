use crate::{
    config::AgentConfig,
    gpu,
    proto::{Host, State},
};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::Instant,
};
use sysinfo::{Disks, Networks, System};

pub struct Monitor {
    system: System,
    networks: Networks,
    disks: Disks,
    previous_net: Option<(Instant, u64, u64)>,
}

impl Monitor {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            previous_net: None,
        }
    }

    pub fn host(&mut self, config: &AgentConfig) -> Host {
        self.system.refresh_memory();
        self.disks.refresh_list();
        let (disk_total, _) = self.disk_usage(config);
        let (platform, platform_version) = crate::platform::platform_info();
        let cpu = cpu_descriptions(&self.system);
        Host {
            platform,
            platform_version,
            cpu,
            mem_total: self.system.total_memory(),
            disk_total,
            swap_total: self.system.total_swap(),
            arch: std::env::consts::ARCH.to_string(),
            virtualization: crate::platform::virtualization(),
            boot_time: System::boot_time(),
            // The dashboard gates MCP capabilities on the agent's semantic
            // version. Keep this tied to the package version so test-only
            // environment variables cannot make a production binary claim a
            // different capability set.
            version: env!("CARGO_PKG_VERSION").to_string(),
            gpu: if config.gpu { gpu::host() } else { Vec::new() },
        }
    }

    pub fn state(&mut self, config: &AgentConfig) -> State {
        self.system.refresh_cpu();
        self.system.refresh_memory();
        if !config.skip_procs_count {
            self.system.refresh_processes();
        }
        self.disks.refresh_list();
        self.networks.refresh();
        let (_, disk_used) = self.disk_usage(config);
        let (mut net_in, mut net_out) = (0, 0);
        for (name, network) in &self.networks {
            if !config.nic_allowlist.is_empty()
                && !config.nic_allowlist.get(name).copied().unwrap_or(false)
            {
                continue;
            }
            if !config.nic_allowlist.get(name).copied().unwrap_or(false)
                && [
                    "lo",
                    "tun",
                    "docker",
                    "veth",
                    "br-",
                    "vmbr",
                    "vnet",
                    "kube",
                    "Meta",
                    "tailscale",
                    "fw",
                    "tap",
                ]
                .iter()
                .any(|part| name.contains(part))
            {
                continue;
            }
            net_in += network.total_received();
            net_out += network.total_transmitted();
        }
        let now = Instant::now();
        let (net_in_speed, net_out_speed) = self
            .previous_net
            .map(|(before, old_in, old_out)| {
                let seconds = now.duration_since(before).as_secs_f64().max(0.001);
                (
                    (net_in.saturating_sub(old_in) as f64 / seconds) as u64,
                    (net_out.saturating_sub(old_out) as f64 / seconds) as u64,
                )
            })
            .unwrap_or((0, 0));
        self.previous_net = Some((now, net_in, net_out));
        let load = System::load_average();
        let (tcp_conn_count, udp_conn_count) = if config.skip_connection_count {
            (0, 0)
        } else {
            crate::platform::connection_counts()
        };
        let temperatures = if config.temperature {
            crate::platform::temperatures()
        } else {
            Vec::new()
        };
        let (gpu_usage, gpus) = if config.gpu {
            gpu::state()
        } else {
            (Vec::new(), Vec::new())
        };
        State {
            cpu: self.system.global_cpu_info().cpu_usage() as f64,
            mem_used: self.system.used_memory(),
            swap_used: crate::platform::swap_used(&self.system),
            disk_used,
            net_in_transfer: net_in,
            net_out_transfer: net_out,
            net_in_speed,
            net_out_speed,
            uptime: System::uptime(),
            load1: load.one,
            load5: load.five,
            load15: load.fifteen,
            tcp_conn_count,
            udp_conn_count,
            process_count: if config.skip_procs_count {
                0
            } else {
                self.system.processes().len() as u64
            },
            temperatures,
            gpu: gpu_usage,
            gpus,
        }
    }

    fn disk_usage(&self, config: &AgentConfig) -> (u64, u64) {
        if !config.hard_drive_partition_allowlist.is_empty() {
            return crate::platform::allowlisted_disk_usage(
                &self.disks,
                &config.hard_drive_partition_allowlist,
            );
        }
        let mut devices = HashSet::new();
        let result = self
            .disks
            .iter()
            .filter(|disk| {
                let filesystem = disk.file_system().to_string_lossy().to_ascii_lowercase();
                supported_filesystem(&filesystem)
                    && !disk
                        .mount_point()
                        .to_string_lossy()
                        .contains("/var/lib/kubelet")
                    && devices.insert(disk.name().to_os_string())
            })
            .map(|disk| {
                crate::platform::filesystem_usage(disk.mount_point()).unwrap_or((
                    disk.total_space(),
                    disk.total_space().saturating_sub(disk.available_space()),
                ))
            })
            .fold((0, 0), |(total, used), (size, consumed)| {
                (total + size, used + consumed)
            });
        if result.0 == 0 {
            crate::platform::filesystem_usage(Path::new("/")).unwrap_or(result)
        } else {
            result
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn disk_allowlist_matches_df_and_cpu_models_are_aggregated() {
        let mut monitor = Monitor::new();
        let config = AgentConfig {
            hard_drive_partition_allowlist: vec!["/tmp".into()],
            ..Default::default()
        };
        let host = monitor.host(&config);
        let output = std::process::Command::new("df")
            .args(["-B1", "-P", "/tmp"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let table = String::from_utf8(output.stdout).unwrap();
        let columns = table
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        let expected_total = columns[1].parse::<u64>().unwrap();
        assert_eq!(host.disk_total, expected_total);
        let expected_models = monitor
            .system
            .cpus()
            .iter()
            .map(|cpu| cpu.brand().trim())
            .filter(|brand| !brand.is_empty())
            .collect::<HashSet<_>>();
        assert_eq!(host.cpu.len(), expected_models.len());
        assert!(host.cpu.iter().all(|item| item.ends_with(" Core")));
    }
}

fn supported_filesystem(name: &str) -> bool {
    matches!(
        name,
        "apfs"
            | "ext4"
            | "ext3"
            | "ext2"
            | "f2fs"
            | "reiserfs"
            | "jfs"
            | "bcachefs"
            | "btrfs"
            | "fuseblk"
            | "zfs"
            | "simfs"
            | "ntfs"
            | "fat32"
            | "exfat"
            | "xfs"
            | "fuse.rclone"
    )
}

fn cpu_descriptions(system: &System) -> Vec<String> {
    let mut models = BTreeMap::<String, usize>::new();
    for cpu in system.cpus() {
        let brand = cpu.brand().trim();
        if !brand.is_empty() {
            *models.entry(brand.to_string()).or_default() += 1;
        }
    }
    if models.len() == 1 {
        if let Some(physical) = system.physical_core_count() {
            *models.values_mut().next().unwrap() = physical;
        }
    }
    let kind = if crate::platform::virtualization().is_empty() {
        "Physical"
    } else {
        "Virtual"
    };
    models
        .into_iter()
        .map(|(name, cores)| format!("{name} {cores} {kind} Core"))
        .collect()
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn advertised_version_passes_dashboard_mcp_gate() {
        let version = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert!(version >= semver::Version::new(2, 1, 0));
        let host = Monitor::new().host(&AgentConfig::default());
        assert_eq!(host.version, version.to_string());
    }
}
