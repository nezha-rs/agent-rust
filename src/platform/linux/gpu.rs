use crate::proto::StateGpu;
use roxmltree::{Document, Node};
use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

enum Vendor {
    Nvidia(PathBuf),
    Amd(PathBuf),
    Intel(PathBuf),
}

fn tool(preferred: &str, name: &str) -> Option<PathBuf> {
    let preferred = Path::new(preferred);
    if preferred.is_file() {
        return Some(preferred.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn vendor() -> &'static Option<Vendor> {
    static DETECTED: OnceLock<Option<Vendor>> = OnceLock::new();
    DETECTED.get_or_init(|| {
        tool("/usr/bin/nvidia-smi", "nvidia-smi")
            .map(Vendor::Nvidia)
            .or_else(|| tool("/opt/rocm/bin/rocm-smi", "rocm-smi").map(Vendor::Amd))
            .or_else(|| tool("/usr/bin/intel_gpu_top", "intel_gpu_top").map(Vendor::Intel))
    })
}

fn output(path: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    output_with_timeout(path, args, PROBE_TIMEOUT)
}

fn output_with_timeout(path: &Path, args: &[&str], timeout: Duration) -> io::Result<Vec<u8>> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    let group = i32::try_from(child.id()).map_err(|_| io::Error::other("invalid GPU probe pid"))?;
    let mut pipe = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        let mut too_large = false;
        loop {
            let count = pipe.read(&mut chunk)?;
            if count == 0 {
                return if too_large {
                    Err(io::Error::other("GPU probe output exceeds 8 MiB"))
                } else {
                    Ok::<_, io::Error>(bytes)
                };
            }
            if !too_large && bytes.len().saturating_add(count) <= MAX_OUTPUT {
                bytes.extend_from_slice(&chunk[..count]);
            } else {
                too_large = true;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = child.wait();
            let _ = reader.join();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GPU probe timed out",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("GPU probe reader panicked"))??;
    if !status.success() {
        return Err(io::Error::other("GPU probe exited unsuccessfully"));
    }
    Ok(bytes)
}

fn child_text<'a>(node: Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.has_tag_name(tag))?
        .text()
}

fn percent(raw: &str) -> f64 {
    raw.trim()
        .trim_end_matches('%')
        .trim()
        .parse()
        .unwrap_or(0.0)
}

fn mib(raw: &str) -> Option<u64> {
    let value = raw.trim().strip_suffix("MiB")?.trim().parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value as u64)
}

fn nvidia(bytes: &[u8]) -> Option<(Vec<String>, Vec<StateGpu>)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let document = Document::parse(text).ok()?;
    let mut names = Vec::new();
    let mut stats = Vec::new();
    for gpu in document
        .descendants()
        .filter(|node| node.has_tag_name("gpu"))
    {
        names.push(
            child_text(gpu, "product_name")
                .unwrap_or_default()
                .to_owned(),
        );
        let utilization = gpu
            .children()
            .find(|node| node.has_tag_name("utilization"))
            .and_then(|node| child_text(node, "gpu_util"))
            .map(percent)
            .unwrap_or_default();
        let memory = gpu
            .children()
            .find(|node| node.has_tag_name("fb_memory_usage"));
        let used = memory
            .and_then(|node| child_text(node, "used"))
            .and_then(mib);
        let total = memory
            .and_then(|node| child_text(node, "total"))
            .and_then(mib);
        stats.push(StateGpu {
            utilization,
            memory_used: used.zip(total).map_or(0, |(used, _)| used),
            memory_total: used.zip(total).map_or(0, |(_, total)| total),
        });
    }
    Some((names, stats))
}

fn amd(bytes: &[u8]) -> Option<(Vec<String>, Vec<StateGpu>)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let cards = value.as_object()?;
    let mut names = Vec::with_capacity(cards.len());
    let mut stats = Vec::with_capacity(cards.len());
    for card in cards.values() {
        names.push(
            card.get("Card series")?
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
        let usage = card.get("GPU use (%)")?;
        let utilization = usage
            .as_f64()
            .or_else(|| usage.as_str()?.parse().ok())
            .unwrap_or_default();
        stats.push(StateGpu {
            utilization,
            memory_used: 0,
            memory_total: 0,
        });
    }
    Some((names, stats))
}

fn intel_headers(first: &str, second: &str) -> Option<(usize, usize)> {
    let engines = first
        .split_whitespace()
        .filter(|column| {
            matches!(
                column.trim_end_matches(|ch: char| ch.is_ascii_digit() || ch == '/'),
                "RCS" | "BCS" | "VCS" | "VECS" | "CCS"
            )
        })
        .count();
    (engines > 0).then(|| {
        let preceding = second
            .split_whitespace()
            .count()
            .saturating_sub(3 * engines);
        (engines, preceding)
    })
}

fn intel_row(line: &str, engines: usize, preceding: usize) -> Option<f64> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < preceding + 3 * engines {
        return None;
    }
    let mut maximum = 0.0_f64;
    for index in 0..engines {
        if let Ok(value) = fields[preceding + 3 * index].parse::<f64>() {
            if value.is_finite() {
                maximum = maximum.max(value);
            }
        }
    }
    Some(maximum)
}

fn intel_stream(reader: impl Read) -> io::Result<f64> {
    let limited = reader.take(MAX_OUTPUT as u64);
    let lines = BufReader::new(limited).lines();
    let mut first_header = String::new();
    let mut columns = None;
    let mut skipped_first = false;
    for line in lines {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Freq") {
            first_header = line.to_owned();
            continue;
        }
        if line.starts_with("req") {
            columns = intel_headers(&first_header, line);
            continue;
        }
        let Some((engines, preceding)) = columns else {
            continue;
        };
        let Some(usage) = intel_row(line, engines, preceding) else {
            continue;
        };
        if !skipped_first {
            skipped_first = true;
            continue;
        }
        return Ok(usage);
    }
    Err(io::Error::other("no valid Intel GPU sample"))
}

fn intel_usage(path: &Path, timeout: Duration) -> io::Result<f64> {
    let mut child = Command::new(path)
        .args(["-s", "1000", "-l"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let group = i32::try_from(child.id()).map_err(|_| io::Error::other("invalid GPU probe pid"))?;
    let pipe = child.stdout.take().expect("piped stdout");
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = sender.send(intel_stream(pipe));
    });
    let result = receiver.recv_timeout(timeout);
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
    reader
        .join()
        .map_err(|_| io::Error::other("Intel GPU reader panicked"))?;
    match result {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Intel GPU probe timed out",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("Intel GPU reader exited"))
        }
    }
}

fn pci_product(ids: &str, device: &str) -> Option<String> {
    let mut intel_section = false;
    for line in ids.lines() {
        if line.starts_with("8086  ") {
            intel_section = true;
        } else if intel_section && !line.starts_with('\t') {
            break;
        } else if intel_section && line.starts_with('\t') && !line.starts_with("\t\t") {
            let Some((id, name)) = line.trim_start_matches('\t').split_once("  ") else {
                continue;
            };
            if id.eq_ignore_ascii_case(device) {
                return Some(name.trim().to_owned());
            }
        }
    }
    None
}

fn intel_models(sysfs: &Path, ids: &str) -> io::Result<Vec<String>> {
    let mut devices: Vec<_> = std::fs::read_dir(sysfs)?.filter_map(Result::ok).collect();
    devices.sort_by_key(|entry| entry.file_name());
    let mut names = Vec::new();
    for entry in devices {
        let path = entry.path();
        let Ok(class) = std::fs::read_to_string(path.join("class")) else {
            continue;
        };
        let Ok(vendor) = std::fs::read_to_string(path.join("vendor")) else {
            continue;
        };
        if !class.trim().starts_with("0x03") || !vendor.trim().eq_ignore_ascii_case("0x8086") {
            continue;
        }
        let Ok(device) = std::fs::read_to_string(path.join("device")) else {
            continue;
        };
        if let Some(name) = pci_product(ids, device.trim().trim_start_matches("0x")) {
            names.push(name);
        }
    }
    Ok(names)
}

fn intel() -> Vec<String> {
    static MODELS: OnceLock<Vec<String>> = OnceLock::new();
    MODELS
        .get_or_init(|| {
            let Some(ids) = ["/usr/share/misc/pci.ids", "/usr/share/hwdata/pci.ids"]
                .iter()
                .find_map(|path| std::fs::read_to_string(path).ok())
            else {
                return Vec::new();
            };
            intel_models(Path::new("/sys/bus/pci/devices"), &ids).unwrap_or_default()
        })
        .clone()
}

fn sample() -> Option<(Vec<String>, Vec<StateGpu>)> {
    match vendor().as_ref()? {
        Vendor::Nvidia(path) => nvidia(&output(path, &["-q", "-x"]).ok()?),
        Vendor::Amd(path) => amd(&output(path, &["-u", "--showproductname", "--json"]).ok()?),
        Vendor::Intel(path) => {
            let utilization = intel_usage(path, PROBE_TIMEOUT).ok()?;
            let names = intel();
            Some((
                names,
                vec![StateGpu {
                    utilization,
                    memory_used: 0,
                    memory_total: 0,
                }],
            ))
        }
    }
}

pub fn host() -> Vec<String> {
    sample().map_or_else(Vec::new, |(names, _)| names)
}

pub fn state() -> (Vec<f64>, Vec<StateGpu>) {
    let stats = sample().map_or_else(Vec::new, |(_, stats)| stats);
    let usage = stats.iter().map(|gpu| gpu.utilization).collect();
    (usage, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("gpu-probe");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (directory, path)
    }

    #[test]
    fn parses_nvidia_models_utilization_and_mib() {
        let xml = br#"<nvidia_smi_log><gpu><product_name>RTX 4090</product_name><utilization><gpu_util> 37 % </gpu_util></utilization><fb_memory_usage><used>1137 MiB</used><total>24564 MiB</total></fb_memory_usage></gpu><gpu><product_name>RTX 6000</product_name><utilization><gpu_util>N/A</gpu_util></utilization><fb_memory_usage><used>N/A</used><total>49140 MiB</total></fb_memory_usage></gpu></nvidia_smi_log>"#;
        let (names, stats) = nvidia(xml).unwrap();
        assert_eq!(names, ["RTX 4090", "RTX 6000"]);
        assert_eq!(stats[0].utilization, 37.0);
        assert_eq!((stats[0].memory_used, stats[0].memory_total), (1137, 24564));
        assert_eq!((stats[1].memory_used, stats[1].memory_total), (0, 0));
    }

    #[test]
    fn parses_amd_cards_without_memory_data() {
        let json = br#"{"card0":{"Card series":"AMD Radeon RX 7900","GPU use (%)":"12"}}"#;
        let (names, stats) = amd(json).unwrap();
        assert_eq!(names, ["AMD Radeon RX 7900"]);
        assert_eq!(stats[0].utilization, 12.0);
        assert_eq!(stats[0].memory_total, 0);
    }

    #[test]
    fn no_gpu_tool_matches_upstream_empty_result() {
        if vendor().is_none() {
            assert!(host().is_empty());
            assert!(state().1.is_empty());
        }
    }

    #[test]
    fn invokes_nvidia_xml_tool_with_upstream_arguments() {
        let (_directory, path) = script("printf '%s\\n' '<nvidia_smi_log><gpu><product_name>Test GPU</product_name></gpu></nvidia_smi_log>'");
        let bytes = output(&path, &["-q", "-x"]).unwrap();
        assert_eq!(nvidia(&bytes).unwrap().0, ["Test GPU"]);
    }

    #[test]
    fn timeout_reaps_probe_descendants() {
        let (_directory, path) = script("sleep 30 & wait");
        let start = Instant::now();
        let error = output_with_timeout(&path, &[], Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn parses_intel_second_sample_and_pci_model() {
        let output = "Freq MHz IRQ RC6 Power RCS/0 BCS/0 VCS/0\nreq act % gpu % % % % % % % % % %\n300 100 0 95 0 1 0 0 2 0 0 3 0 0\n300 100 0 95 0 7 0 0 42 0 0 11 0 0\n";
        assert_eq!(intel_stream(output.as_bytes()).unwrap(), 42.0);
        let directory = tempfile::tempdir().unwrap();
        let card = directory.path().join("0000:00:02.0");
        std::fs::create_dir(&card).unwrap();
        std::fs::write(card.join("class"), "0x030000\n").unwrap();
        std::fs::write(card.join("vendor"), "0x8086\n").unwrap();
        std::fs::write(card.join("device"), "0x1234\n").unwrap();
        let ids =
            "8086  Intel Corporation\n\t1234  Intel Fixture Graphics\n10de  NVIDIA Corporation\n";
        assert_eq!(
            intel_models(directory.path(), ids).unwrap(),
            ["Intel Fixture Graphics"]
        );
    }

    #[test]
    fn intel_stream_stops_and_reaps_continuous_probe() {
        let body = "printf 'Freq MHz RCS/0\\nreq act x x busy sema wait\\n1 1 1 1 99 0 0\\n1 1 1 1 23 0 0\\n'; sleep 30 & wait";
        let (_directory, path) = script(body);
        let start = Instant::now();
        assert_eq!(intel_usage(&path, Duration::from_secs(2)).unwrap(), 23.0);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn intel_continuous_probe_times_out() {
        let (_directory, path) = script("sleep 30 & wait");
        let start = Instant::now();
        let error = intel_usage(&path, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
