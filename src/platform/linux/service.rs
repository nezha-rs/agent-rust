#[cfg(target_os = "linux")]
mod systemd {
    use super::*;
    use crate::config::AgentConfig;
    use anyhow::bail;
    use std::{fs, process::Command};

    pub fn control(args: &Args, action: ServiceAction, user: bool) -> Result<()> {
        let executable = std::env::current_exe().context("locate executable")?;
        let config = absolute_config_path(args, &executable)?;
        let name = service_name(&executable, &config)?;
        let unit = format!("{name}.service");
        let unit_path = if user {
            let home = std::env::var_os("HOME").context("HOME is required for --user")?;
            PathBuf::from(home).join(".config/systemd/user").join(&unit)
        } else {
            PathBuf::from("/etc/systemd/system").join(&unit)
        };

        match action {
            ServiceAction::Install => {
                if !config.is_file() {
                    bail!("configuration file does not exist: {}", config.display());
                }
                // Service invocations only receive -c, so validate the file without CLI overrides.
                let check_args = Args {
                    config: Some(config.clone()),
                    server: None,
                    client_secret: None,
                    uuid: None,
                    command: None,
                };
                AgentConfig::load(&check_args).context("validate service configuration")?;
                fs::create_dir_all(unit_path.parent().context("unit directory")?)?;
                let content = unit_content(&executable, &config, &name, user)?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&unit_path)
                    .with_context(|| format!("create {}", unit_path.display()))?;
                use std::io::Write;
                if let Err(error) = file.write_all(content.as_bytes()) {
                    let _ = fs::remove_file(&unit_path);
                    return Err(error).context("write systemd unit");
                }
                drop(file);
                if let Err(error) = verify_unit(user, &unit_path)
                    .and_then(|_| systemctl(user, &["enable", &unit]))
                    .and_then(|_| systemctl(user, &["daemon-reload"]))
                {
                    let _ = systemctl(user, &["disable", &unit]);
                    let _ = fs::remove_file(&unit_path);
                    let _ = systemctl(user, &["daemon-reload"]);
                    return Err(error);
                }
                println!("installed {unit}");
            }
            ServiceAction::Uninstall => {
                systemctl(user, &["disable", &unit])?;
                fs::remove_file(&unit_path)
                    .with_context(|| format!("remove {}", unit_path.display()))?;
                systemctl(user, &["daemon-reload"])?;
                println!("uninstalled {unit}");
            }
            ServiceAction::Start => systemctl(user, &["start", &unit])?,
            ServiceAction::Stop => systemctl(user, &["stop", &unit])?,
            ServiceAction::Restart => systemctl(user, &["restart", &unit])?,
        }
        Ok(())
    }

    fn systemctl(user: bool, args: &[&str]) -> Result<()> {
        let mut cmd = Command::new("systemctl");
        if user {
            cmd.arg("--user");
        }
        let output = cmd.args(args).output().context("run systemctl")?;
        if !output.status.success() {
            bail!(
                "systemctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn verify_unit(user: bool, path: &Path) -> Result<()> {
        let mut cmd = Command::new("systemd-analyze");
        cmd.arg("verify");
        if user {
            cmd.arg("--user");
        }
        let output = cmd.arg(path).output().context("run systemd-analyze")?;
        if !output.status.success() {
            bail!(
                "invalid systemd unit: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn unit_content(executable: &Path, config: &Path, name: &str, user: bool) -> Result<String> {
        let directory = unit_path_value(executable.parent().context("executable has no parent")?)?;
        let condition = unit_path_value(executable)?;
        let executable = unit_quote(executable)?;
        let config = unit_quote(config)?;
        let wanted_by = if user {
            "default.target"
        } else {
            "multi-user.target"
        };
        Ok(format!(
            "[Unit]\nDescription=Nezha Monitoring Agent\nConditionFileIsExecutable={condition}\n\n[Service]\nStartLimitInterval=5\nStartLimitBurst=10\nExecStart={executable} -c {config}\nWorkingDirectory={directory}\nRestart=always\nRestartSec=30\nEnvironmentFile=-/etc/sysconfig/{name}\n\n[Install]\nWantedBy={wanted_by}\n"
        ))
    }

    fn unit_path_value(path: &Path) -> Result<String> {
        let value = path.to_str().context("systemd path is not UTF-8")?;
        if !path.is_absolute() || value.contains(['\n', '\r', '\0']) {
            bail!("systemd path must be absolute and contain no control characters");
        }
        Ok(value
            .replace('\\', "\\x5c")
            .replace(' ', "\\x20")
            .replace('%', "%%"))
    }

    fn unit_quote(path: &Path) -> Result<String> {
        let value = path.to_str().context("systemd path is not UTF-8")?;
        if value.contains(['\n', '\r', '\0']) {
            bail!("systemd path contains an invalid character");
        }
        Ok(format!(
            "\"{}\"",
            value
                .replace('%', "%%")
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn service_name_matches_go_config_path_hash() {
            let executable = Path::new("/opt/nezha/nezha-agent");
            assert_eq!(
                service_name(executable, Path::new("/opt/nezha/config.yml")).unwrap(),
                "nezha-agent"
            );
            assert_eq!(
                service_name(executable, Path::new("/tmp/nezha.yml")).unwrap(),
                "nezha-agent-38a27f9"
            );
        }

        #[test]
        fn unit_escapes_systemd_specifiers_and_quotes() {
            assert_eq!(
                unit_quote(Path::new("/tmp/a %b\"c")).unwrap(),
                "\"/tmp/a %%b\\\"c\""
            );
            assert_eq!(
                unit_path_value(Path::new("/tmp/a %b")).unwrap(),
                "/tmp/a\\x20%%b"
            );
        }
    }
}
