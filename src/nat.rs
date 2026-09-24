use crate::{
    authenticated,
    config::AgentConfig,
    proto::{nezha_service_client::NezhaServiceClient, IoStreamData, Task},
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

#[derive(Deserialize)]
struct NatTask {
    #[serde(rename = "StreamID", alias = "stream_id")]
    stream_id: String,
    #[serde(rename = "Host", alias = "host")]
    host: String,
}

pub async fn run(
    task: Task,
    config: &AgentConfig,
    mut client: NezhaServiceClient<Channel>,
) -> Result<()> {
    if config.disable_nat {
        return Ok(());
    }
    let request: NatTask = serde_json::from_str(&task.data).context("parse NAT task")?;
    if request.stream_id.is_empty() || request.host.is_empty() {
        bail!("NAT stream ID and host are required");
    }
    let (sender, receiver) = mpsc::channel::<IoStreamData>(32);
    let mut attach = vec![0xff, 0x05, 0xff, 0x05];
    attach.extend_from_slice(request.stream_id.as_bytes());
    sender.send(IoStreamData { data: attach }).await?;
    let mut stream = client
        .io_stream(authenticated(ReceiverStream::new(receiver), config)?)
        .await
        .context("open NAT IOStream")?
        .into_inner();

    let (host, port) = crate::endpoint::split_host_port(&request.host)
        .map_err(anyhow::Error::msg)
        .context("invalid NAT backend address")?;
    let tcp = time::timeout(
        Duration::from_secs(10),
        crate::platform::connect_tcp_host(host, port, &config.dns),
    )
    .await
    .context("NAT backend dial timed out")?
    .with_context(|| format!("dial NAT backend {}", request.host))?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let mut sender = Some(sender);
    let mut keepalive = time::interval(Duration::from_secs(30));
    keepalive.tick().await;
    let mut data = [0_u8; 32 * 1024];
    let mut local_eof = false;
    let mut drain_deadline = None;

    loop {
        tokio::select! {
            remote = stream.message() => {
                match remote.context("receive NAT IOStream")? {
                    Some(frame) if !frame.data.is_empty() => tcp_write.write_all(&frame.data).await?,
                    Some(_) => {},
                    None => break,
                }
            }
            read = tcp_read.read(&mut data), if !local_eof => {
                match read {
                    Ok(0) => {
                        finish_local_read(&mut sender, "EOF").await?;
                        local_eof = true;
                        drain_deadline = Some(time::Instant::now() + Duration::from_secs(30));
                    }
                    Ok(count) => {
                        sender.as_ref().expect("local read is active")
                            .send(IoStreamData { data: data[..count].to_vec() }).await?;
                    }
                    Err(error) => {
                        finish_local_read(&mut sender, &error.to_string()).await?;
                        local_eof = true;
                        drain_deadline = Some(time::Instant::now() + Duration::from_secs(30));
                    }
                }
            }
            _ = keepalive.tick(), if sender.is_some() => {
                sender.as_ref().expect("send direction is open")
                    .send(IoStreamData { data: Vec::new() }).await?;
            }
            _ = time::sleep_until(drain_deadline.unwrap_or_else(|| time::Instant::now() + Duration::from_secs(3600))), if drain_deadline.is_some() => break,
        }
    }
    tcp_write.shutdown().await?;
    Ok(())
}

async fn finish_local_read(
    sender: &mut Option<mpsc::Sender<IoStreamData>>,
    reason: &str,
) -> Result<()> {
    if let Some(open) = sender.take() {
        open.send(IoStreamData {
            data: reason.as_bytes().to_vec(),
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dashboard_task_field_names() {
        let task: NatTask =
            serde_json::from_str(r#"{"StreamID":"abc","Host":"127.0.0.1:80"}"#).unwrap();
        assert_eq!(task.stream_id, "abc");
        assert_eq!(task.host, "127.0.0.1:80");
    }

    #[tokio::test]
    async fn backend_eof_sends_legacy_frame_then_closes_send_direction() {
        let (sender, mut receiver) = mpsc::channel(2);
        let mut sender = Some(sender);
        sender
            .as_ref()
            .unwrap()
            .send(IoStreamData {
                data: b"tail".to_vec(),
            })
            .await
            .unwrap();
        finish_local_read(&mut sender, "EOF").await.unwrap();
        assert!(sender.is_none());
        assert_eq!(receiver.recv().await.unwrap().data, b"tail");
        assert_eq!(receiver.recv().await.unwrap().data, b"EOF");
        assert!(receiver.recv().await.is_none());
    }
}
