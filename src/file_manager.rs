use crate::{
    authenticated,
    config::AgentConfig,
    proto::{nezha_service_client::NezhaServiceClient, IoStreamData, Task},
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{ffi::OsString, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time,
};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::transport::Channel;

#[derive(Deserialize)]
struct FileManagerTask {
    #[serde(rename = "StreamID", alias = "stream_id")]
    stream_id: String,
}

async fn send(sender: &mpsc::Sender<IoStreamData>, data: Vec<u8>) -> Result<()> {
    sender.send(IoStreamData { data }).await?;
    Ok(())
}

async fn send_error(
    sender: &mpsc::Sender<IoStreamData>,
    error: impl std::fmt::Display,
) -> Result<()> {
    let mut frame = b"NERR".to_vec();
    frame.extend_from_slice(error.to_string().as_bytes());
    send(sender, frame).await
}

fn list_frame(path: &Path) -> Result<Vec<u8>> {
    let mut entries = std::fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            Ok((entry.file_name(), entry.file_type()?.is_dir()))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    encode_list_frame(path, &mut entries)
}

fn encode_list_frame(path: &Path, entries: &mut [(OsString, bool)]) -> Result<Vec<u8>> {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let label = path.to_string_lossy();
    let mut frame = b"NZFN".to_vec();
    frame.extend_from_slice(&u32::try_from(label.len())?.to_be_bytes());
    frame.extend_from_slice(label.as_bytes());
    for (name, is_dir) in entries {
        let name = name.to_string_lossy();
        frame.push(u8::from(*is_dir));
        frame.push(name.len() as u8);
        frame.extend_from_slice(name.as_bytes());
    }
    Ok(frame)
}

async fn download(path: String, sender: mpsc::Sender<IoStreamData>) -> Result<()> {
    let file = crate::platform::open_legacy_download_file(Path::new(&path))?;
    let size = file.metadata()?.len();
    if size == 0 {
        bail!("requested file is empty");
    }
    let mut header = b"NZTD".to_vec();
    header.extend_from_slice(&size.to_be_bytes());
    send(&sender, header).await?;
    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        send(&sender, buffer[..count].to_vec()).await?;
    }
    Ok(())
}

pub async fn run(
    task: Task,
    config: &AgentConfig,
    mut client: NezhaServiceClient<Channel>,
) -> Result<()> {
    if config.disable_command_execute {
        return Ok(());
    }
    let request: FileManagerTask = serde_json::from_str(&task.data).context("parse FM task")?;
    if request.stream_id.is_empty() {
        bail!("FM stream ID is required");
    }
    let (sender, receiver) = mpsc::channel::<IoStreamData>(32);
    let mut attach = vec![0xff, 0x05, 0xff, 0x05];
    attach.extend_from_slice(request.stream_id.as_bytes());
    send(&sender, attach).await?;
    let stream = client
        .io_stream(authenticated(ReceiverStream::new(receiver), config)?)
        .await
        .context("open FM IOStream")?
        .into_inner();
    drive_stream(stream, sender).await
}

async fn drive_stream<S>(mut stream: S, sender: mpsc::Sender<IoStreamData>) -> Result<()>
where
    S: Stream<Item = std::result::Result<IoStreamData, tonic::Status>> + Unpin,
{
    let mut keepalive = time::interval(Duration::from_secs(30));
    keepalive.tick().await;
    let mut downloads = tokio::task::JoinSet::new();
    let mut upload: Option<(tokio::fs::File, u64)> = None;
    let outcome: Result<()> = async {
        loop {
            tokio::select! {
                remote = stream.next() => {
                    let frame = match remote {
                        Some(Ok(frame)) => frame,
                        Some(Err(error)) => {
                            if let Some((file, _)) = upload.take() {
                                let _ = file.sync_all().await;
                                let _ = send_error(&sender, error.to_string()).await;
                            }
                            return Err(error).context("receive FM IOStream");
                        }
                        None => {
                            if let Some((file, _)) = upload.take() {
                                file.sync_all().await?;
                                send_error(&sender, "EOF").await?;
                            }
                            break;
                        }
                    };
                    if frame.data.is_empty() { continue; }
                    if let Some((file, remaining)) = upload.as_mut() {
                        if let Err(error) = file.write_all(&frame.data).await {
                            upload.take();
                            send_error(&sender, error).await?;
                            continue;
                        }
                        *remaining = remaining.saturating_sub(frame.data.len() as u64);
                        if *remaining == 0 {
                            let (file, _) = upload.take().unwrap();
                            match file.sync_all().await {
                                Ok(()) => send(&sender, b"NZUP".to_vec()).await?,
                                Err(error) => send_error(&sender, error).await?,
                            }
                        }
                        continue;
                    }
                    let command = frame.data[0];
                    match command {
                        0 => {
                            let path = String::from_utf8_lossy(&frame.data[1..]).to_string();
                            let requested = Path::new(&path);
                            let listing = list_frame(requested).or_else(|_| {
                                let home = crate::platform::user_home()
                                    .context("current user home is unavailable")?;
                                list_frame(&home)
                            });
                            match listing {
                                Ok(data) => send(&sender, data).await?,
                                Err(error) => send_error(&sender, error).await?,
                            }
                        }
                        1 => {
                            let path = String::from_utf8_lossy(&frame.data[1..]).to_string();
                            let sender = sender.clone();
                            downloads.spawn(async move {
                                if let Err(error) = download(path, sender.clone()).await {
                                    let _ = send_error(&sender, error).await;
                                }
                            });
                        }
                        2 => {
                            if frame.data.len() < 9 {
                                send_error(&sender, "data is invalid").await?;
                                continue;
                            }
                            let size = u64::from_be_bytes(frame.data[1..9].try_into()?);
                            let path = String::from_utf8_lossy(&frame.data[9..]).to_string();
                            match tokio::fs::File::create(&path).await {
                                Ok(file) if size == 0 => {
                                    match file.sync_all().await {
                                        Ok(()) => send(&sender, b"NZUP".to_vec()).await?,
                                        Err(error) => send_error(&sender, error).await?,
                                    }
                                }
                                Ok(file) => upload = Some((file, size)),
                                Err(error) => send_error(&sender, error).await?,
                            }
                        }
                        _ => {}
                    }
                }
                _ = keepalive.tick() => send(&sender, Vec::new()).await?,
            }
        }
        Ok(())
    }
    .await;
    downloads.abort_all();
    while downloads.join_next().await.is_some() {}
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_wire_format_matches_go_agent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"x").unwrap();
        let frame = list_frame(dir.path()).unwrap();
        assert_eq!(&frame[..4], b"NZFN");
        let path_size = u32::from_be_bytes(frame[4..8].try_into().unwrap()) as usize;
        assert_eq!(
            &frame[8..8 + path_size],
            dir.path().to_string_lossy().as_bytes()
        );
        assert_eq!(&frame[8 + path_size..], b"\0\x01a");
    }

    #[test]
    fn directory_frame_uses_snapshot_when_entry_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("disappearing");
        std::fs::write(&entry, b"x").unwrap();
        let mut snapshot = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), entry.file_type().unwrap().is_dir())
            })
            .collect::<Vec<_>>();
        std::fs::remove_file(entry).unwrap();
        let frame = encode_list_frame(dir.path(), &mut snapshot).unwrap();
        assert!(frame.ends_with(b"\0\x0cdisappearing"));
    }

    #[tokio::test]
    async fn truncated_upload_keeps_partial_file_and_sends_legacy_error() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("truncated.bin");
        let mut command = vec![2];
        command.extend_from_slice(&8_u64.to_be_bytes());
        command.extend_from_slice(target.to_string_lossy().as_bytes());
        let input = tokio_stream::iter([
            Ok(IoStreamData { data: command }),
            Ok(IoStreamData {
                data: b"part".to_vec(),
            }),
        ]);
        let (sender, mut output) = mpsc::channel(4);
        drive_stream(input, sender).await.unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"part");
        assert_eq!(output.recv().await.unwrap().data, b"NERREOF");
    }

    #[tokio::test]
    async fn failed_upload_keeps_partial_file_and_sends_legacy_error() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("failed.bin");
        let mut command = vec![2];
        command.extend_from_slice(&8_u64.to_be_bytes());
        command.extend_from_slice(target.to_string_lossy().as_bytes());
        let input = tokio_stream::iter([
            Ok(IoStreamData { data: command }),
            Ok(IoStreamData {
                data: b"part".to_vec(),
            }),
            Err(tonic::Status::cancelled("peer disconnected")),
        ]);
        let (sender, mut output) = mpsc::channel(4);
        assert!(drive_stream(input, sender).await.is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"part");
        assert!(output.recv().await.unwrap().data.starts_with(b"NERR"));
    }

    #[tokio::test]
    async fn legacy_upload_accepts_full_oversend_frame() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("oversend.bin");
        let mut command = vec![2];
        command.extend_from_slice(&4_u64.to_be_bytes());
        command.extend_from_slice(target.to_string_lossy().as_bytes());
        let input = tokio_stream::iter([
            Ok(IoStreamData { data: command }),
            Ok(IoStreamData {
                data: b"body-plus-extra".to_vec(),
            }),
        ]);
        let (sender, mut output) = mpsc::channel(4);
        drive_stream(input, sender).await.unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"body-plus-extra");
        assert_eq!(output.recv().await.unwrap().data, b"NZUP");
    }

    #[tokio::test]
    async fn missing_directory_lists_current_user_home() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing");
        let mut command = vec![0];
        command.extend_from_slice(path.to_string_lossy().as_bytes());
        let input = tokio_stream::iter([Ok(IoStreamData { data: command })]);
        let (sender, mut output) = mpsc::channel(4);
        drive_stream(input, sender).await.unwrap();
        let frame = output.recv().await.unwrap().data;
        assert_eq!(&frame[..4], b"NZFN");
        let size = u32::from_be_bytes(frame[4..8].try_into().unwrap()) as usize;
        let home = crate::platform::user_home().unwrap();
        assert_eq!(&frame[8..8 + size], home.to_string_lossy().as_bytes());
    }
}
