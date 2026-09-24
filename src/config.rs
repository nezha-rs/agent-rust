use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(version, about = "Nezha monitoring agent (Rust)")]
pub struct Args {
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,
    #[arg(long, env = "NZ_SERVER")]
    pub server: Option<String>,
    #[arg(long, env = "NZ_CLIENT_SECRET")]
    pub client_secret: Option<String>,
    #[arg(long, env = "NZ_UUID")]
    pub uuid: Option<Uuid>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Edit,
    Service {
        #[command(subcommand)]
        action: ServiceAction,
        /// Manage a systemd user unit instead of a system unit.
        #[arg(long, global = true)]
        user: bool,
    },
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    #[serde(skip)]
    pub config_path: PathBuf,
    pub debug: bool,
    pub server: String,
    pub client_secret: String,
    pub uuid: String,
    pub hard_drive_partition_allowlist: Vec<String>,
    pub nic_allowlist: HashMap<String, bool>,
    pub dns: Vec<String>,
    pub gpu: bool,
    pub temperature: bool,
    pub skip_connection_count: bool,
    pub skip_procs_count: bool,
    pub disable_auto_update: bool,
    pub disable_force_update: bool,
    pub disable_command_execute: bool,
    pub report_delay: u32,
    pub tls: bool,
    pub insecure_tls: bool,
    pub use_ipv6_country_code: bool,
    pub use_gitee_to_upgrade: bool,
    pub use_atomgit_to_upgrade: bool,
    pub disable_nat: bool,
    pub disable_send_query: bool,
    pub ip_report_period: u32,
    pub self_update_period: u32,
    pub rust_update_manifest_url: String,
    pub rust_gitee_update_manifest_url: String,
    pub rust_atomgit_update_manifest_url: String,
    pub custom_ip_api: Vec<String>,
}

fn default_rust_update_manifest_url() -> String {
    option_env!("NEZHA_DEFAULT_UPDATE_MANIFEST_URL")
        .unwrap_or_default()
        .to_owned()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            config_path: PathBuf::new(),
            debug: false,
            server: String::new(),
            client_secret: String::new(),
            uuid: String::new(),
            hard_drive_partition_allowlist: Vec::new(),
            nic_allowlist: HashMap::new(),
            dns: Vec::new(),
            gpu: false,
            temperature: false,
            skip_connection_count: false,
            skip_procs_count: false,
            disable_auto_update: false,
            disable_force_update: false,
            disable_command_execute: false,
            report_delay: 0,
            tls: false,
            insecure_tls: false,
            use_ipv6_country_code: false,
            use_gitee_to_upgrade: false,
            use_atomgit_to_upgrade: false,
            disable_nat: false,
            disable_send_query: false,
            ip_report_period: 0,
            self_update_period: 0,
            rust_update_manifest_url: default_rust_update_manifest_url(),
            rust_gitee_update_manifest_url: String::new(),
            rust_atomgit_update_manifest_url: String::new(),
            custom_ip_api: Vec::new(),
        }
    }
}

impl AgentConfig {
    pub fn load(args: &Args) -> Result<Self> {
        let path = args.config.clone().unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("nezha-agent-rust"))
                .with_file_name("config.yml")
        });
        let first_run = !path.exists();
        let mut config: Self = if !first_run {
            serde_yaml::from_slice(&fs::read(&path).context("read config")?)
                .context("parse config.yml")?
        } else {
            Self::default()
        };
        // Match the Go agent's NZ_ environment override behavior.
        let mut value = serde_json::to_value(&config)?;
        if let Some(map) = value.as_object_mut() {
            for (key, raw) in std::env::vars().filter(|(key, _)| key.starts_with("NZ_")) {
                let field = key.trim_start_matches("NZ_").to_ascii_lowercase();
                if let Some(old) = map.get(&field) {
                    let parsed = if old.is_boolean() {
                        serde_json::Value::Bool(raw.parse().with_context(|| key.clone())?)
                    } else if old.is_number() {
                        serde_json::Value::Number(raw.parse::<u64>()?.into())
                    } else if old.is_array() || old.is_object() {
                        serde_json::from_str(&raw).with_context(|| key.clone())?
                    } else {
                        serde_json::Value::String(raw)
                    };
                    map.insert(field, parsed);
                }
            }
        }
        config = serde_json::from_value(value)?;
        config.config_path = path.clone();
        if let Some(server) = &args.server {
            config.server.clone_from(server);
        }
        if let Some(secret) = &args.client_secret {
            config.client_secret.clone_from(secret);
        }
        if let Some(uuid) = args.uuid {
            config.uuid = uuid.to_string();
        }
        let generated_uuid = config.uuid.is_empty();
        if generated_uuid {
            config.uuid = Uuid::new_v4().to_string();
        }
        config.validate(false)?;
        if first_run || generated_uuid {
            config.save()?;
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.config_path, serde_yaml::to_string(self)?)?;
        crate::platform::secure_config_file(&self.config_path)?;
        Ok(())
    }

    pub fn validate(&mut self, is_remote: bool) -> Result<()> {
        if !is_remote && (self.server.is_empty() || self.client_secret.is_empty()) {
            bail!("server and client_secret must be configured");
        }
        if !is_remote {
            Uuid::parse_str(&self.uuid).context("invalid uuid")?;
        }
        if self.report_delay == 0 {
            self.report_delay = 3;
        }
        if !(1..=4).contains(&self.report_delay) {
            bail!("report-delay ranges from 1-4");
        }
        if self.ip_report_period == 0 {
            self.ip_report_period = 1800;
        } else {
            self.ip_report_period = self.ip_report_period.max(30);
        }
        for api in &self.custom_ip_api {
            let parsed = reqwest::Url::parse(api)
                .with_context(|| format!("invalid custom_ip_api: {api}"))?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                bail!("custom_ip_api entry {api:?} must use http or https scheme");
            }
        }
        for (field, raw) in [
            ("rust_update_manifest_url", &self.rust_update_manifest_url),
            (
                "rust_gitee_update_manifest_url",
                &self.rust_gitee_update_manifest_url,
            ),
            (
                "rust_atomgit_update_manifest_url",
                &self.rust_atomgit_update_manifest_url,
            ),
        ] {
            if raw.is_empty() {
                continue;
            }
            let url = reqwest::Url::parse(raw).with_context(|| format!("invalid {field}"))?;
            if url.scheme() != "https"
                && !(url.scheme() == "http"
                    && matches!(
                        url.host_str(),
                        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                    ))
            {
                bail!("{field} must use HTTPS or loopback HTTP");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_url_defaults_only_when_missing() {
        let missing: AgentConfig = serde_yaml::from_str("server: localhost:8008").unwrap();
        assert_eq!(
            missing.rust_update_manifest_url,
            default_rust_update_manifest_url()
        );
        if option_env!("NEZHA_DEFAULT_UPDATE_MANIFEST_URL").is_some() {
            assert!(!missing.rust_update_manifest_url.is_empty());
        }
        let disabled: AgentConfig = serde_yaml::from_str("rust_update_manifest_url: ''").unwrap();
        assert!(disabled.rust_update_manifest_url.is_empty());
    }

    #[test]
    fn first_run_with_explicit_uuid_persists_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.yml");
        let uuid = Uuid::parse_str("00000000-0000-0000-0000-000000000555").unwrap();
        let config = AgentConfig::load(&Args {
            config: Some(path.clone()),
            server: Some("127.0.0.1:8008".into()),
            client_secret: Some("integration-test-secret".into()),
            uuid: Some(uuid),
            command: None,
        })
        .unwrap();
        assert!(path.exists());
        let saved: AgentConfig = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.uuid, uuid.to_string());
        assert_eq!(saved.server, config.server);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
