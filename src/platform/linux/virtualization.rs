use std::{fs, path::Path, sync::OnceLock};

pub(crate) fn virtualization() -> String {
    static SYSTEM: OnceLock<String> = OnceLock::new();
    SYSTEM
        .get_or_init(|| detect(Path::new("/proc"), Path::new("/etc"), Path::new("/")))
        .clone()
}

fn read(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).into_owned()
}

fn detect(proc_root: &Path, etc_root: &Path, host_root: &Path) -> String {
    let mut system = "";
    let mut role = "";

    if proc_root.join("xen").exists() {
        system = "xen";
        role = if read(&proc_root.join("xen/capabilities"))
            .lines()
            .any(|line| line == "control_d")
        {
            "host"
        } else {
            "guest"
        };
    }

    let modules = read(&proc_root.join("modules"));
    if modules.contains("kvm") {
        system = "kvm";
        role = "host";
    } else if modules.contains("hv_util") {
        system = "hyperv";
        role = "guest";
    } else if modules.contains("vboxdrv") {
        system = "vbox";
        role = "host";
    } else if modules.contains("vboxguest") {
        system = "vbox";
        role = "guest";
    } else if modules.contains("vmware") {
        system = "vmware";
        role = "guest";
    }

    let cpuinfo = read(&proc_root.join("cpuinfo"));
    if [
        "QEMU Virtual CPU",
        "Common KVM processor",
        "Common 32-bit KVM processor",
    ]
    .iter()
    .any(|marker| cpuinfo.contains(marker))
    {
        system = "kvm";
        role = "guest";
    }
    if read(&proc_root.join("bus/pci/devices")).contains("virtio-pci") {
        role = "guest";
    }

    if proc_root.join("bc/0").exists() {
        system = "openvz";
        role = "host";
    } else if proc_root.join("vz").exists() {
        system = "openvz";
        role = "guest";
    }
    let status = read(&proc_root.join("self/status"));
    if status.contains("s_context:") || status.contains("VxID:") {
        system = "linux-vserver";
    }
    if read(&proc_root.join("1/environ")).contains("container=lxc") {
        system = "lxc";
        role = "guest";
    }
    let cgroup = read(&proc_root.join("self/cgroup"));
    if cgroup.contains("lxc") {
        system = "lxc";
        role = "guest";
    } else if cgroup.contains("docker") {
        system = "docker";
        role = "guest";
    } else if cgroup.contains("machine-rkt") {
        system = "rkt";
        role = "guest";
    } else if proc_root.join("self/cgroup").exists()
        && host_root.join("usr/bin/lxc-version").exists()
    {
        system = "lxc";
        role = "host";
    }

    if read(&etc_root.join("os-release"))
        .lines()
        .any(|line| matches!(line, "ID=coreos" | "ID=\"coreos\""))
    {
        system = "rkt";
        role = "host";
    }
    if host_root.join(".dockerenv").exists() {
        system = "docker";
        role = "guest";
    }
    if role == "guest" {
        system.into()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_guest_and_host_roles_in_order() {
        let root = tempfile::tempdir().unwrap();
        let proc_root = root.path().join("proc");
        let etc_root = root.path().join("etc");
        fs::create_dir_all(proc_root.join("bus/pci")).unwrap();
        fs::create_dir_all(proc_root.join("self")).unwrap();
        fs::create_dir_all(&etc_root).unwrap();

        fs::write(proc_root.join("modules"), "kvm_intel 123 0\n").unwrap();
        assert_eq!(detect(&proc_root, &etc_root, root.path()), "");
        fs::write(proc_root.join("bus/pci/devices"), "virtio-pci\n").unwrap();
        assert_eq!(detect(&proc_root, &etc_root, root.path()), "kvm");

        fs::write(proc_root.join("modules"), "hv_util 123 0\n").unwrap();
        assert_eq!(detect(&proc_root, &etc_root, root.path()), "hyperv");
        fs::write(proc_root.join("self/cgroup"), "0::/docker/abc\n").unwrap();
        assert_eq!(detect(&proc_root, &etc_root, root.path()), "docker");
        fs::write(root.path().join(".dockerenv"), "").unwrap();
        fs::write(proc_root.join("self/cgroup"), "0::/init.scope\n").unwrap();
        assert_eq!(detect(&proc_root, &etc_root, root.path()), "docker");
    }

    #[test]
    fn detects_xen_guest_without_modules() {
        let root = tempfile::tempdir().unwrap();
        let proc_root = root.path().join("proc");
        fs::create_dir_all(proc_root.join("xen")).unwrap();
        assert_eq!(
            detect(&proc_root, &root.path().join("etc"), root.path()),
            "xen"
        );
        fs::write(proc_root.join("xen/capabilities"), "control_d\n").unwrap();
        assert_eq!(
            detect(&proc_root, &root.path().join("etc"), root.path()),
            ""
        );
    }
}
