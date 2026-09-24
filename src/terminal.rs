use crate::{
    authenticated,
    config::AgentConfig,
    proto::{nezha_service_client::NezhaServiceClient, IoStreamData, Task},
};
use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, PtySize};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    sync::Mutex,
    time::Duration,
};
use tokio::{sync::mpsc, time};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

#[derive(Deserialize)]
struct TerminalTask {
    #[serde(rename = "StreamID", alias = "stream_id")]
    stream_id: String,
}

#[derive(Deserialize)]
struct WindowSize {
    #[serde(rename = "Cols", alias = "cols")]
    cols: u16,
    #[serde(rename = "Rows", alias = "rows")]
    rows: u16,
}

fn valid_size(size: &WindowSize) -> bool {
    (2..=1000).contains(&size.cols) && (2..=500).contains(&size.rows)
}

struct KillChild(Box<dyn Child + Send + Sync>);

impl Drop for KillChild {
    fn drop(&mut self) {
        crate::platform::terminate_pty_child(self.0.as_mut());
    }
}

pub async fn run(
    task: Task,
    config: &AgentConfig,
    mut client: NezhaServiceClient<Channel>,
) -> Result<()> {
    if config.disable_command_execute {
        return Ok(());
    }
    let request: TerminalTask = serde_json::from_str(&task.data).context("parse terminal task")?;
    if request.stream_id.is_empty() {
        bail!("terminal stream ID is required");
    }
    let (sender, receiver) = mpsc::channel::<IoStreamData>(32);
    let mut attach = vec![0xff, 0x05, 0xff, 0x05];
    attach.extend_from_slice(request.stream_id.as_bytes());
    sender.send(IoStreamData { data: attach }).await?;

    let mut stream = client
        .io_stream(authenticated(ReceiverStream::new(receiver), config)?)
        .await
        .context("open terminal IOStream")?
        .into_inner();

    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let child = KillChild(
        pair.slave
            .spawn_command(crate::platform::terminal_command()?)?,
    );
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = Mutex::new(pair.master);
    let (output_tx, mut output_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>(32);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = output_tx.blocking_send(Err(std::io::Error::other("EOF")));
                    break;
                }
                Ok(count) => {
                    if output_tx
                        .blocking_send(Ok(buffer[..count].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = output_tx.blocking_send(Err(error));
                    break;
                }
            }
        }
    });
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(32);
    let writer_task = tokio::task::spawn_blocking(move || {
        while let Some(data) = input_rx.blocking_recv() {
            if writer.write_all(&data).is_err() {
                break;
            }
        }
    });
    let mut keepalive = time::interval(Duration::from_secs(30));
    keepalive.tick().await;

    let outcome: Result<()> = async {
        loop {
            tokio::select! {
                remote = stream.message() => {
                    let frame = match remote.context("receive terminal IOStream")? {
                        Some(frame) => frame,
                        None => break,
                    };
                    match frame.data.split_first() {
                        Some((&0, data)) => input_tx.send(data.to_vec()).await?,
                        Some((&1, data)) => {
                            if let Ok(size) = serde_json::from_slice::<WindowSize>(data) {
                                if valid_size(&size) {
                                    master.lock().unwrap_or_else(|poison| poison.into_inner()).resize(PtySize {
                                        rows: size.rows,
                                        cols: size.cols,
                                        pixel_width: 0,
                                        pixel_height: 0,
                                    })?;
                                }
                            }
                        }
                        _ => {},
                    }
                }
                output = output_rx.recv() => {
                    match output {
                        Some(Ok(data)) => sender.send(IoStreamData { data }).await?,
                        Some(Err(error)) => {
                            sender.send(IoStreamData { data: error.to_string().into_bytes() }).await?;
                            break;
                        }
                        None => break,
                    }
                }
                _ = keepalive.tick() => {
                    sender.send(IoStreamData { data: Vec::new() }).await?;
                }
            }
        }
        Ok(())
    }
    .await;
    drop(child);
    drop(input_tx);
    drop(output_rx);
    drop(master);
    let _ = time::timeout(Duration::from_secs(2), reader_task).await;
    let _ = time::timeout(Duration::from_secs(2), writer_task).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_payload_and_resize_bounds() {
        let task: TerminalTask = serde_json::from_str(r#"{"StreamID":"abc"}"#).unwrap();
        assert_eq!(task.stream_id, "abc");
        let size: WindowSize = serde_json::from_str(r#"{"Cols":80,"Rows":24}"#).unwrap();
        assert!(valid_size(&size));
    }
}
