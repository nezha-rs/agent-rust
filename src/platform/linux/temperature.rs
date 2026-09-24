use crate::proto::StateSensorTemperature;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn temperatures() -> Vec<StateSensorTemperature> {
    read_temperatures(Path::new("/sys"))
}

fn read_temperatures(sys: &Path) -> Vec<StateSensorTemperature> {
    let hwmon = sys.join("class/hwmon");
    let mut inputs = collect_inputs(&hwmon, false);
    if inputs.is_empty() {
        inputs = collect_inputs(&hwmon, true);
    }
    let mut values = if inputs.is_empty() {
        thermal_zones(sys)
    } else {
        inputs
            .into_iter()
            .filter_map(|input| {
                let directory = input.parent()?;
                let stem = input.file_name()?.to_string_lossy();
                let stem = stem.split('_').next()?;
                let mut name = fs::read_to_string(directory.join("name"))
                    .ok()?
                    .trim()
                    .to_string();
                let label = fs::read_to_string(directory.join(format!("{stem}_label")))
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase()
                    .replace(' ', "_");
                if !label.is_empty() {
                    name.push('_');
                    name.push_str(&label);
                }
                let temperature = read_millidegrees(&input)?;
                Some(StateSensorTemperature { name, temperature })
            })
            .collect()
    };
    values.retain(|sensor| {
        sensor.temperature > 0.0 && sensor.name != "PMU tcal" && sensor.name != "noname"
    });
    values.sort_by(|a, b| a.name.cmp(&b.name));
    values
}

fn collect_inputs(hwmon: &Path, device_directory: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(hwmon) else {
        return files;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("hwmon") {
            continue;
        }
        let directory = if device_directory {
            entry.path().join("device")
        } else {
            entry.path()
        };
        if let Ok(inputs) = fs::read_dir(directory) {
            for input in inputs.flatten() {
                let name = input.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("temp") && name.ends_with("_input") {
                    files.push(input.path());
                }
            }
        }
    }
    files.sort();
    files
}

fn thermal_zones(sys: &Path) -> Vec<StateSensorTemperature> {
    let mut zones = Vec::new();
    if let Ok(entries) = fs::read_dir(sys.join("class/thermal")) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("thermal_zone")
            {
                zones.push(entry.path());
            }
        }
    }
    zones.sort();
    zones
        .into_iter()
        .filter_map(|zone| {
            let name = fs::read_to_string(zone.join("type"))
                .ok()?
                .trim()
                .to_string();
            let temperature = read_millidegrees(&zone.join("temp"))?;
            Some(StateSensorTemperature { name, temperature })
        })
        .collect()
}

fn read_millidegrees(path: &Path) -> Option<f64> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|value| value / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwmon_labels_and_fallback_match_go_sensor_keys() {
        let root = tempfile::tempdir().unwrap();
        let hwmon = root.path().join("class/hwmon/hwmon0");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("name"), "coretemp\n").unwrap();
        fs::write(hwmon.join("temp1_label"), "Core 0\n").unwrap();
        fs::write(hwmon.join("temp1_input"), "42125\n").unwrap();
        let values = read_temperatures(root.path());
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].name, "coretemp_core_0");
        assert_eq!(values[0].temperature, 42.125);

        fs::remove_file(hwmon.join("temp1_input")).unwrap();
        let device = hwmon.join("device");
        fs::create_dir(&device).unwrap();
        fs::write(device.join("name"), "nvme\n").unwrap();
        fs::write(device.join("temp1_input"), "37000\n").unwrap();
        assert_eq!(read_temperatures(root.path())[0].name, "nvme");

        fs::remove_file(device.join("temp1_input")).unwrap();
        let zone = root.path().join("class/thermal/thermal_zone0");
        fs::create_dir_all(&zone).unwrap();
        fs::write(zone.join("type"), "acpitz\n").unwrap();
        fs::write(zone.join("temp"), "39000\n").unwrap();
        assert_eq!(read_temperatures(root.path())[0].name, "acpitz");
    }

    #[test]
    fn shared_hwmon_fixture_matches_upstream_probe() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/temperature-fixture");
        let values = read_temperatures(&root);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].name, "coretemp_core_0");
        assert_eq!(values[0].temperature, 42.125);
    }
}
