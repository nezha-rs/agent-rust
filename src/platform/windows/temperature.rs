use crate::proto::StateSensorTemperature;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Cache {
    values: Vec<StateSensorTemperature>,
    running: bool,
}

pub(crate) fn temperatures() -> Vec<StateSensorTemperature> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if !guard.running {
        guard.running = true;
        std::thread::spawn(move || {
            let values = query_temperatures();
            let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
            guard.values = values;
            guard.running = false;
        });
    }
    guard.values.clone()
}

fn query_temperatures() -> Vec<StateSensorTemperature> {
    // Keep the WMI probe outside the report path, as the Go agent does.
    let mut child = match Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance -Namespace root/wmi -ClassName MSAcpi_ThermalZoneTemperature -ErrorAction Stop | Select-Object InstanceName,CurrentTemperature | ConvertTo-Json -Compress",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };
    let stdout = child.stdout.take().expect("piped WMI stdout");
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Vec::new();
                }
                return output
                    .join()
                    .ok()
                    .and_then(Result::ok)
                    .map_or_else(Vec::new, |bytes| parse_temperatures(&bytes));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
}

fn parse_temperatures(output: &[u8]) -> Vec<StateSensorTemperature> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let items = match value {
        serde_json::Value::Array(items) => items,
        item => vec![item],
    };
    let mut values = items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("InstanceName")?.as_str()?.to_string();
            let decikelvin = item.get("CurrentTemperature")?.as_f64()?;
            let temperature = decikelvin / 10.0 - 273.15;
            (temperature > 0.0 && name != "PMU tcal" && name != "noname")
                .then_some(StateSensorTemperature { name, temperature })
        })
        .collect::<Vec<_>>();
    values.sort_by(|a, b| a.name.cmp(&b.name));
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmi_temperature_keys_and_units_match_go() {
        let single = parse_temperatures(
            br#"{"InstanceName":"ACPI\\ThermalZone\\THM0","CurrentTemperature":3052}"#,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].name, r"ACPI\ThermalZone\THM0");
        assert!((single[0].temperature - 32.05).abs() < 0.001);
        let multiple = parse_temperatures(
            br#"[{"InstanceName":"noname","CurrentTemperature":3100},{"InstanceName":"B","CurrentTemperature":3000},{"InstanceName":"A","CurrentTemperature":3010}]"#,
        );
        assert_eq!(
            multiple
                .iter()
                .map(|sensor| sensor.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"]
        );
    }
}
