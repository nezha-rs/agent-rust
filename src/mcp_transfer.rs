use crate::{
    authenticated,
    config::AgentConfig,
    mcp_fs,
    platform::{TransferTarget, TransferTemp},
    proto::{nezha_service_client::NezhaServiceClient, IoStreamData, Task},
    AbortOnDrop,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time,
};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::transport::Channel;

const MAX_SIZE: i64 = 100 * 1024 * 1024;
const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Deserialize)]
struct TransferRequest {
    stream_id: String,
    op: String,
    path: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    create_dirs: bool,
    #[serde(default)]
    if_match_sha256: String,
    #[serde(default)]
    expected_sha256: String,
}

fn header(magic: &[u8; 4], size: u64, digest: Option<&[u8; 32]>) -> Vec<u8> {
    let mut result = Vec::with_capacity(44);
    result.extend_from_slice(magic);
    result.extend_from_slice(&size.to_be_bytes());
    if let Some(digest) = digest {
        result.extend_from_slice(digest);
    }
    result
}

async fn send(sender: &mpsc::Sender<IoStreamData>, data: Vec<u8>) -> Result<()> {
    sender.send(IoStreamData { data }).await?;
    Ok(())
}

async fn send_error(sender: &mpsc::Sender<IoStreamData>, error: impl std::fmt::Display) {
    let mut data = b"NZTE".to_vec();
    data.extend_from_slice(error.to_string().as_bytes());
    let _ = time::timeout(Duration::from_secs(2), send(sender, data)).await;
}

fn file_hash(target: &TransferTarget) -> Result<String> {
    let mut file = target.open_read()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn check_match(target: &TransferTarget, expected: &str) -> Result<()> {
    if expected.is_empty() {
        return Ok(());
    }
    let actual = match file_hash(target) {
        Ok(hash) => hash,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            bail!("if_match precondition failed: file does not exist");
        }
        Err(error) => return Err(error),
    };
    if actual != expected {
        bail!("if_match precondition failed: sha256 mismatch");
    }
    Ok(())
}

async fn upload<S>(
    request: &TransferRequest,
    path: &Path,
    target: &TransferTarget,
    temporary: TransferTemp,
    stream: Option<&mut S>,
    sender: &mpsc::Sender<IoStreamData>,
) -> Result<[u8; 32]>
where
    S: Stream<Item = std::result::Result<IoStreamData, tonic::Status>> + Unpin,
{
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let mut hasher = Sha256::new();
    let mut remaining = request.size as usize;
    let mut keepalive = time::interval(Duration::from_secs(30));
    keepalive.tick().await;
    let mut stream = if remaining > 0 {
        Some(stream.context("upload response stream is required")?)
    } else {
        None
    };
    while remaining > 0 {
        tokio::select! {
            incoming = stream.as_mut().unwrap().next() => {
                let frame = incoming.context("upload ended before declared size")?
                    .context("receive transfer upload")?;
                if frame.data.is_empty() {
                    continue;
                }
                if frame.data.len() > remaining {
                    bail!("upload oversend: payload exceeds declared remaining size");
                }
                output.write_all(&frame.data).await?;
                hasher.update(&frame.data);
                remaining -= frame.data.len();
            }
            _ = keepalive.tick() => send(sender, Vec::new()).await?,
        }
    }
    output.sync_all().await?;
    drop(output);
    let digest: [u8; 32] = hasher.finalize().into();
    if !request.expected_sha256.is_empty() {
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != request.expected_sha256 {
            bail!("sha256 mismatch");
        }
    }
    crate::platform::apply_transfer_mode(temporary.as_file(), &request.mode)
        .context("invalid mode")?;
    temporary.as_file().sync_all()?;
    let _guard = mcp_fs::locks()[mcp_fs::stripe(path)]
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    check_match(target, &request.if_match_sha256)?;
    target.commit(temporary)?;
    Ok(digest)
}

async fn download(file: File, size: u64, sender: &mpsc::Sender<IoStreamData>) -> Result<[u8; 32]> {
    let mut input = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    while remaining > 0 {
        let count = input
            .read(&mut buffer[..remaining.min(CHUNK_SIZE as u64) as usize])
            .await?;
        if count == 0 {
            bail!("source truncated mid-transfer");
        }
        hasher.update(&buffer[..count]);
        let mut frame = header(b"NZTC", count as u64, None);
        frame.extend_from_slice(&buffer[..count]);
        send(sender, frame).await?;
        remaining -= count as u64;
    }
    Ok(hasher.finalize().into())
}

pub async fn run(
    task: Task,
    config: &AgentConfig,
    mut client: NezhaServiceClient<Channel>,
) -> Result<()> {
    let request: TransferRequest =
        serde_json::from_str(&task.data).context("parse transfer task")?;
    if request.stream_id.is_empty() {
        bail!("transfer stream ID is required");
    }
    let (sender, receiver) = mpsc::channel::<IoStreamData>(32);
    let mut attach = vec![0xff, 0x05, 0xff, 0x05];
    attach.extend_from_slice(request.stream_id.as_bytes());
    send(&sender, attach).await?;

    let path = mcp_fs::resolve(&request.path).map_err(anyhow::Error::msg);
    let mut upload_target = None;
    let mut upload_file = None;
    let mut download_file = None;
    let mut size = 0_u64;
    let early_error = if config.disable_command_execute {
        Some("agent disabled file operations".to_string())
    } else {
        match (&request.op[..], &path) {
            ("upload", Ok(path)) => {
                if mcp_fs::is_root(path) {
                    Some("refusing to write to filesystem root".into())
                } else if !(0..=MAX_SIZE).contains(&request.size) {
                    Some("size out of range: must be 0..100MiB".into())
                } else {
                    let setup: Result<_> = (|| {
                        if !request.if_match_sha256.is_empty() {
                            let initial = TransferTarget::new(path, false).map_err(|error| {
                                if error.kind() == std::io::ErrorKind::NotFound {
                                    anyhow::anyhow!(
                                        "if_match precondition failed: file does not exist"
                                    )
                                } else {
                                    error.into()
                                }
                            })?;
                            check_match(&initial, &request.if_match_sha256)?;
                        }
                        if !request.mode.is_empty() {
                            u32::from_str_radix(&request.mode, 8).context("invalid mode")?;
                        }
                        let target = TransferTarget::new(path, request.create_dirs)?;
                        let file = target.temporary()?;
                        Ok((target, file))
                    })();
                    match setup {
                        Ok((target, file)) => {
                            upload_target = Some(target);
                            upload_file = Some(file);
                            size = request.size as u64;
                            None
                        }
                        Err(error) => Some(error.to_string()),
                    }
                }
            }
            ("download", Ok(path)) => {
                match TransferTarget::new(path, false).and_then(|target| target.open_read()) {
                    Ok(file) => match file.metadata() {
                        Ok(meta) if meta.len() <= MAX_SIZE as u64 => {
                            size = meta.len();
                            download_file = Some(file);
                            None
                        }
                        Ok(_) => Some("file exceeds MCP transfer cap (100MiB)".into()),
                        Err(error) => Some(error.to_string()),
                    },
                    Err(error) => Some(error.to_string()),
                }
            }
            ("upload" | "download", Err(error)) => Some(error.to_string()),
            _ => Some(format!("unknown op: {}", request.op)),
        }
    };
    if let Some(error) = &early_error {
        send_error(&sender, error).await;
    } else if request.op == "upload" {
        send(&sender, header(b"NZTU", size, None)).await?;
    } else {
        send(&sender, header(b"NZTD", size, Some(&[0; 32]))).await?;
    }

    let grpc_request = authenticated(ReceiverStream::new(receiver), config)?;
    let mut open = AbortOnDrop(tokio::spawn(
        async move { client.io_stream(grpc_request).await },
    ));
    if early_error.is_some() {
        drop(sender);
        let _ = time::timeout(Duration::from_secs(10), &mut open.0).await;
        open.0.abort();
        return Ok(());
    }

    let mut response_stream = None;
    let mut open_consumed = false;
    let outcome: Result<[u8; 32]> = if request.op == "upload" {
        async {
            if size > 0 {
                let opened = time::timeout(Duration::from_secs(300), &mut open.0)
                    .await
                    .context("transfer stream timed out")?;
                open_consumed = true;
                response_stream = Some(opened.context("join transfer IOStream")??.into_inner());
            }
            time::timeout(
                Duration::from_secs(300),
                upload(
                    &request,
                    path.as_ref().unwrap(),
                    upload_target.as_ref().unwrap(),
                    upload_file.take().unwrap(),
                    response_stream.as_mut(),
                    &sender,
                ),
            )
            .await
            .context("upload timed out")?
        }
        .await
    } else {
        time::timeout(Duration::from_secs(300), async {
            let digest = download(download_file.take().unwrap(), size, &sender).await?;
            send(&sender, header(b"NZTO", size, Some(&digest))).await?;
            Ok(digest)
        })
        .await
        .context("download timed out")?
    };
    match outcome {
        Ok(digest) => {
            if request.op == "upload" {
                send(&sender, header(b"NZTO", size, Some(&digest))).await?;
            }
        }
        Err(error) => send_error(&sender, error).await,
    }
    drop(sender);
    if response_stream.is_none() && !open_consumed {
        response_stream = time::timeout(Duration::from_secs(10), &mut open.0)
            .await
            .ok()
            .and_then(|result| result.ok())
            .and_then(|result| result.ok())
            .map(|response| response.into_inner());
    }
    if let Some(mut stream) = response_stream {
        let _ = time::timeout(Duration::from_secs(10), async {
            while let Ok(Some(_)) = stream.message().await {}
        })
        .await;
    }
    open.0.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_frames_match_current_go_magic() {
        assert_eq!(header(b"NZTU", 5, None), b"NZTU\0\0\0\0\0\0\0\x05");
        assert_eq!(header(b"NZTO", 0, Some(&[0; 32])).len(), 44);
    }

    #[test]
    fn transfer_mode_is_applied_by_target_specialist() {
        let file = tempfile::NamedTempFile::new().unwrap();
        crate::platform::apply_transfer_mode(file.as_file(), "0444").unwrap();
        assert!(file.as_file().metadata().unwrap().permissions().readonly());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hash_precondition_uses_original_parent_after_rename() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let replacement = root.path().join("replacement");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(parent.join("data"), b"first").unwrap();
        std::fs::write(replacement.join("data"), b"second").unwrap();
        let target = TransferTarget::new(&parent.join("data"), false).unwrap();
        std::fs::rename(&parent, root.path().join("moved")).unwrap();
        std::fs::rename(&replacement, &parent).unwrap();
        assert!(check_match(&target, &format!("{:x}", Sha256::digest(b"first"))).is_ok());
        assert!(check_match(&target, &format!("{:x}", Sha256::digest(b"second"))).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn failed_uploads_preserve_target_and_remove_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("target");
        std::fs::write(&path, b"original").unwrap();
        for frames in [
            vec![Ok(IoStreamData {
                data: b"part".to_vec(),
            })],
            vec![Ok(IoStreamData {
                data: b"too-many".to_vec(),
            })],
        ] {
            let target = TransferTarget::new(&path, false).unwrap();
            let temporary = target.temporary().unwrap();
            let temp_path = temporary.path().to_owned();
            let mut stream = tokio_stream::iter(frames);
            let (sender, _receiver) = mpsc::channel(4);
            let request = TransferRequest {
                stream_id: "test".into(),
                op: "upload".into(),
                path: path.to_string_lossy().into_owned(),
                size: 5,
                mode: String::new(),
                create_dirs: false,
                if_match_sha256: String::new(),
                expected_sha256: String::new(),
            };
            assert!(upload(
                &request,
                &path,
                &target,
                temporary,
                Some(&mut stream),
                &sender,
            )
            .await
            .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"original");
            assert!(!temp_path.exists());
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelled_stalled_upload_removes_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("target");
        std::fs::write(&path, b"original").unwrap();
        let target = TransferTarget::new(&path, false).unwrap();
        let temporary = target.temporary().unwrap();
        let temp_path = temporary.path().to_owned();
        let request = TransferRequest {
            stream_id: "stalled".into(),
            op: "upload".into(),
            path: path.to_string_lossy().into_owned(),
            size: 5,
            mode: String::new(),
            create_dirs: false,
            if_match_sha256: String::new(),
            expected_sha256: String::new(),
        };
        let (sender, _receiver) = mpsc::channel(4);
        let job = tokio::spawn(async move {
            let mut stream =
                tokio_stream::pending::<std::result::Result<IoStreamData, tonic::Status>>();
            upload(
                &request,
                &path,
                &target,
                temporary,
                Some(&mut stream),
                &sender,
            )
            .await
        });
        tokio::task::yield_now().await;
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        assert!(!temp_path.exists());
        assert_eq!(
            std::fs::read(root.path().join("target")).unwrap(),
            b"original"
        );
    }
}
