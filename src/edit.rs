use crate::config::{AgentConfig, Args};
use anyhow::{bail, Context, Result};
use std::{
    io::{self, BufRead, Write},
    net::SocketAddr,
};
use sysinfo::{Disks, Networks};

pub fn run(args: &Args) -> Result<()> {
    let mut config = AgentConfig::load(args)?;
    let networks = Networks::new_with_refreshed_list();
    let mut nics: Vec<String> = networks.keys().cloned().collect();
    nics.sort();
    let disks = Disks::new_with_refreshed_list();
    let mut mounts: Vec<String> = disks
        .iter()
        .map(|disk| disk.mount_point().to_string_lossy().into_owned())
        .collect();
    mounts.sort();
    mounts.dedup();
    let stdin = io::stdin();
    let stdout = io::stdout();
    edit(
        &mut config,
        &nics,
        &mounts,
        &mut stdin.lock(),
        &mut stdout.lock(),
    )?;
    config.save()?;
    println!("Configuration saved. Restart the agent to apply changes.");
    Ok(())
}

fn edit(
    config: &mut AgentConfig,
    nics: &[String],
    mounts: &[String],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let chosen_nics = select("Network interfaces", nics, input, output)?;
    let chosen_mounts = select("Disk partitions", mounts, input, output)?;
    let dns = prompt(
        &format!("DNS servers (comma-separated) [{}]", config.dns.join(",")),
        input,
        output,
    )?;
    let dns = if dns.is_empty() {
        config.dns.clone()
    } else if dns == "-" {
        Vec::new()
    } else {
        parse_dns(&dns)?
    };
    let uuid = prompt(&format!("Agent UUID [{}]", config.uuid), input, output)?;
    let uuid = if uuid.is_empty() {
        config.uuid.clone()
    } else {
        uuid::Uuid::parse_str(&uuid)
            .context("invalid agent UUID")?
            .to_string()
    };
    let gpu = confirm("Enable GPU monitoring", config.gpu, input, output)?;
    let temperature = confirm(
        "Enable temperature monitoring",
        config.temperature,
        input,
        output,
    )?;
    let debug = confirm("Enable debug logging", config.debug, input, output)?;

    config.nic_allowlist = chosen_nics.into_iter().map(|name| (name, true)).collect();
    config.hard_drive_partition_allowlist = chosen_mounts;
    config.dns = dns;
    config.uuid = uuid;
    config.gpu = gpu;
    config.temperature = temperature;
    config.debug = debug;
    config.validate(false)?;
    Ok(())
}

fn prompt(label: &str, input: &mut impl BufRead, output: &mut impl Write) -> Result<String> {
    write!(output, "{label}: ")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("configuration edit cancelled");
    }
    Ok(line.trim().to_owned())
}

fn select(
    label: &str,
    options: &[String],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Vec<String>> {
    writeln!(output, "{label}:")?;
    for (index, option) in options.iter().enumerate() {
        writeln!(output, "  {}. {option}", index + 1)?;
    }
    let answer = prompt(
        "Choose numbers separated by commas (blank for all)",
        input,
        output,
    )?;
    if answer.is_empty() {
        return Ok(Vec::new());
    }
    let mut selected = Vec::new();
    for item in answer.split(',') {
        let index: usize = item.trim().parse().context("invalid selection number")?;
        let option = options
            .get(index.checked_sub(1).context("selection starts at 1")?)
            .context("selection is outside the displayed list")?;
        if !selected.contains(option) {
            selected.push(option.clone());
        }
    }
    Ok(selected)
}

fn confirm(
    label: &str,
    current: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool> {
    let answer = prompt(
        &format!("{label} (y/n, default {})", if current { "y" } else { "n" }),
        input,
        output,
    )?;
    match answer.to_ascii_lowercase().as_str() {
        "" => Ok(current),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => bail!("expected y or n for {label}"),
    }
}

fn parse_dns(raw: &str) -> Result<Vec<String>> {
    raw.split(',')
        .map(|entry| {
            let entry = entry.trim();
            let _: SocketAddr = entry
                .parse()
                .with_context(|| format!("invalid DNS address: {entry}"))?;
            Ok(entry.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn interactive_edit_updates_only_selected_fields() {
        let mut config = AgentConfig {
            server: "localhost:8008".into(),
            client_secret: "original-secret".into(),
            uuid: "00000000-0000-0000-0000-000000000001".into(),
            ..Default::default()
        };
        let input = b"2\n1\n1.1.1.1:53,[::1]:5353\n\ny\ny\nn\n";
        edit(
            &mut config,
            &["eth0".into(), "wlan0".into()],
            &["/mnt/data".into()],
            &mut &input[..],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            config.nic_allowlist,
            HashMap::from([("wlan0".into(), true)])
        );
        assert_eq!(config.hard_drive_partition_allowlist, ["/mnt/data"]);
        assert_eq!(config.dns, ["1.1.1.1:53", "[::1]:5353"]);
        assert_eq!(config.client_secret, "original-secret");
        assert!(config.gpu && config.temperature && !config.debug);
    }

    #[test]
    fn invalid_answer_leaves_original_configuration_unchanged() {
        let mut config = AgentConfig::default();
        let before = serde_yaml::to_string(&config).unwrap();
        let input = b"1\n\ninvalid-dns\n";
        assert!(edit(
            &mut config,
            &["eth0".into()],
            &[],
            &mut &input[..],
            &mut Vec::new(),
        )
        .is_err());
        assert_eq!(serde_yaml::to_string(&config).unwrap(), before);
    }
}
