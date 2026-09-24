use crate::config::AgentConfig;
use anyhow::{bail, Context, Result};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path, sync::OnceLock, time::Duration};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

#[derive(Deserialize)]
struct Manifest {
    version: String,
    assets: HashMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
}

static UPDATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

pub async fn check(config: &AgentConfig, forced: bool) -> Result<()> {
    if config.use_gitee_to_upgrade && config.rust_gitee_update_manifest_url.is_empty() {
        eprintln!("Gitee Rust update mirror is not configured; using another configured source");
    } else if config.use_atomgit_to_upgrade && config.rust_atomgit_update_manifest_url.is_empty() {
        eprintln!("AtomGit Rust update mirror is not configured; using another configured source");
    }
    let source = manifest_source(config);
    if source.is_empty() {
        if forced {
            eprintln!("forced update ignored: no Rust update manifest URL is configured");
        }
        return Ok(());
    }
    let _guard = UPDATE_LOCK.get_or_init(|| Mutex::new(())).lock().await;
    let manifest_url = checked_url(source)?;
    let client = crate::platform::http_client_builder(
        reqwest::Client::builder().timeout(Duration::from_secs(30)),
        &config.dns,
    )?
    .build()?;
    let mut manifest_response = client.get(manifest_url).send().await?.error_for_status()?;
    if manifest_response.content_length().unwrap_or(0) > 1024 * 1024 {
        bail!("Rust update manifest exceeds 1 MiB");
    }
    let mut manifest_bytes = Vec::new();
    while let Some(chunk) = manifest_response.chunk().await? {
        if manifest_bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            bail!("Rust update manifest exceeds 1 MiB");
        }
        manifest_bytes.extend_from_slice(&chunk);
    }
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse Rust update manifest")?;
    let available = Version::parse(manifest.version.trim_start_matches('v'))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    if available <= current && (!forced || available <= Version::new(0, 1, 0)) {
        return Ok(());
    }
    let asset = manifest
        .assets
        .get(env!("NEZHA_BUILD_TARGET"))
        .context("release does not contain this Rust target")?;
    let expected = sha256_hex(&asset.sha256)?;
    let url = checked_url(&asset.url)?;
    let directory = tempfile::tempdir().context("create update staging directory")?;
    let name = crate::platform::update_executable_name();
    let path = directory.path().join(name);
    download(&client, url, &path, &expected).await?;
    crate::platform::prepare_update_executable(&path)?;
    let version_line = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&path)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("downloaded Rust executable version check timed out")?
    .context("validate downloaded Rust executable")?;
    let expected_banner = format!("nezha-agent-rust {available}");
    if !version_line.status.success()
        || String::from_utf8_lossy(&version_line.stdout).trim() != expected_banner
    {
        bail!("downloaded file is not the expected Rust agent version");
    }
    let path_for_replace = path.clone();
    tokio::task::spawn_blocking(move || self_replace::self_replace(path_for_replace)).await??;
    eprintln!("updated Rust agent to {available}; exiting for restart");
    std::process::exit(1);
}

fn manifest_source(config: &AgentConfig) -> &str {
    if config.use_gitee_to_upgrade && !config.rust_gitee_update_manifest_url.is_empty() {
        &config.rust_gitee_update_manifest_url
    } else if config.use_atomgit_to_upgrade && !config.rust_atomgit_update_manifest_url.is_empty() {
        &config.rust_atomgit_update_manifest_url
    } else {
        &config.rust_update_manifest_url
    }
}

fn checked_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw)?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1")))
    {
        bail!("update URL must use HTTPS or loopback HTTP");
    }
    Ok(url)
}

fn sha256_hex(raw: &str) -> Result<[u8; 32]> {
    if raw.len() != 64 {
        bail!("update SHA-256 must contain 64 hex digits");
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16)?;
    }
    Ok(digest)
}

async fn download(
    client: &reqwest::Client,
    url: reqwest::Url,
    path: &Path,
    expected: &[u8; 32],
) -> Result<()> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut file = fs::File::create(path).await?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response.chunk().await? {
        size += chunk.len() as u64;
        if size > MAX_DOWNLOAD {
            bail!("Rust update asset exceeds 256 MiB");
        }
        digest.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    if size == 0 || digest.finalize().as_slice() != expected {
        bail!("Rust update SHA-256 mismatch or empty asset");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_untrusted_update_locations_and_hashes() {
        assert!(checked_url("http://example.com/manifest.json").is_err());
        assert!(checked_url("http://127.0.0.1:18542/manifest.json").is_ok());
        assert!(sha256_hex("00").is_err());
        assert_eq!(sha256_hex(&"ab".repeat(32)).unwrap(), [0xab; 32]);
    }

    #[test]
    fn mirror_flags_select_configured_rust_release_manifests() {
        let mut config = AgentConfig {
            rust_update_manifest_url: "https://github.test/manifest.json".into(),
            rust_gitee_update_manifest_url: "https://gitee.test/manifest.json".into(),
            rust_atomgit_update_manifest_url: "https://atomgit.test/manifest.json".into(),
            ..Default::default()
        };
        assert_eq!(manifest_source(&config), config.rust_update_manifest_url);
        config.use_atomgit_to_upgrade = true;
        assert_eq!(
            manifest_source(&config),
            config.rust_atomgit_update_manifest_url
        );
        config.use_gitee_to_upgrade = true;
        assert_eq!(
            manifest_source(&config),
            config.rust_gitee_update_manifest_url
        );
        config.rust_gitee_update_manifest_url.clear();
        assert_eq!(
            manifest_source(&config),
            config.rust_atomgit_update_manifest_url
        );
    }
}
