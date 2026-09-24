#[cfg(target_os = "macos")]
mod launchd {
    use super::*;
    use crate::config::AgentConfig;
    use anyhow::bail;
    use std::{fs, process::Command};

    pub fn control(args: &Args, action: ServiceAction, user: bool) -> Result<()> {
        let executable = std::env::current_exe().context("locate executable")?;
        let config = absolute_config_path(args, &executable)?;
        let name = service_name(&executable, &config)?;
        let (domain, path) = if user {
            let home = std::env::var_os("HOME").context("HOME is required for --user")?;
            (
                format!("gui/{}", unsafe { libc::getuid() }),
                PathBuf::from(home)
                    .join("Library/LaunchAgents")
                    .join(format!("{name}.plist")),
            )
        } else {
            (
                "system".into(),
                PathBuf::from("/Library/LaunchDaemons").join(format!("{name}.plist")),
            )
        };
        let label = &name;
        match action {
            ServiceAction::Install => {
                if !config.is_file() {
                    bail!("configuration file does not exist: {}", config.display());
                }
                let check_args = Args {
                    config: Some(config.clone()),
                    server: None,
                    client_secret: None,
                    uuid: None,
                    command: None,
                };
                AgentConfig::load(&check_args).context("validate service configuration")?;
                fs::create_dir_all(path.parent().context("plist directory")?)?;
                let plist = plist_content(label, &executable, &config)?;
                fs::write(&path, plist).with_context(|| format!("write {}", path.display()))?;
                launchctl(&["bootstrap", &domain, path.to_str().context("plist path")?])?;
                println!("installed {label}");
            }
            ServiceAction::Uninstall => {
                let _ = launchctl(&["bootout", &domain, &path.to_string_lossy()]);
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
                println!("uninstalled {label}");
            }
            ServiceAction::Start => {
                launchctl(&["bootstrap", &domain, path.to_str().context("plist path")?])?
            }
            ServiceAction::Stop => launchctl(&["bootout", &format!("{domain}/{label}")])?,
            ServiceAction::Restart => {
                launchctl(&["kickstart", "-k", &format!("{domain}/{label}")])?
            }
        }
        Ok(())
    }

    fn launchctl(args: &[&str]) -> Result<()> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .context("run launchctl")?;
        if !output.status.success() {
            bail!(
                "launchctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn plist_content(label: &str, executable: &Path, config: &Path) -> Result<String> {
        let xml = |value: &str| {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let directory = xml(executable
            .parent()
            .context("executable directory")?
            .to_str()
            .context("executable directory is not UTF-8")?);
        let executable = xml(executable.to_str().context("executable path")?);
        let config = xml(config.to_str().context("config path")?);
        let label = xml(label);
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{executable}</string><string>-c</string><string>{config}</string></array><key>WorkingDirectory</key><string>{directory}</string><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>30</integer></dict></plist>\n"
        ))
    }
}
