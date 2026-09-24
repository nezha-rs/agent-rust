mod config;
mod edit;
mod endpoint;
mod file_manager;
mod geoip;
mod gpu;
mod insecure_tls;
mod mcp_exec;
mod mcp_fs;
mod mcp_transfer;
mod monitor;
mod nat;
mod platform;
mod runtime;
mod service;
mod tasks;
mod terminal;
mod updater;

#[allow(clippy::result_large_err)]
pub mod proto {
    tonic::include_proto!("proto");
}

use anyhow::{Context, Result};
use clap::Parser;
use config::{AgentConfig, Args, Command};
use hyper_util::rt::TokioIo;
use monitor::Monitor;
use proto::{nezha_service_client::NezhaServiceClient, TaskResult};
use runtime::Runtime;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    metadata::MetadataValue,
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request,
};
use tower::service_fn;

pub(crate) struct AbortOnDrop<T = ()>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn authenticated<T>(message: T, config: &AgentConfig) -> Result<Request<T>> {
    let mut request = Request::new(message);
    let metadata = request.metadata_mut();
    let secret = MetadataValue::try_from(config.client_secret.as_str())?;
    let uuid = MetadataValue::try_from(config.uuid.as_str())?;
    metadata.insert("client-secret", secret.clone());
    metadata.insert("client-uuid", uuid.clone());
    metadata.insert("client_secret", secret);
    metadata.insert("client_uuid", uuid);
    Ok(request)
}

async fn connect(config: &AgentConfig) -> Result<NezhaServiceClient<Channel>> {
    let scheme = if config.tls && !config.insecure_tls {
        "https"
    } else {
        "http"
    };
    let url = format!("{scheme}://{}", config.server);
    let mut endpoint = Endpoint::from_shared(url)?
        .connect_timeout(Duration::from_secs(10))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true);
    if config.tls {
        if config.insecure_tls {
            eprintln!("WARNING: TLS certificate verification is disabled (insecure_tls=true)");
            return Ok(NezhaServiceClient::new(
                insecure_tls::connect(endpoint, &config.dns).await?,
            ));
        }
        endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
    }
    let channel = if !cfg!(target_os = "linux") && config.dns.is_empty() {
        endpoint.connect().await?
    } else {
        let servers = config.dns.clone();
        endpoint
            .connect_with_connector(service_fn(move |uri: http::Uri| {
                let servers = servers.clone();
                async move {
                    let host = uri.host().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "missing service host",
                        )
                    })?;
                    let port = uri
                        .port_u16()
                        .unwrap_or(if uri.scheme_str() == Some("https") {
                            443
                        } else {
                            80
                        });
                    Ok::<_, std::io::Error>(TokioIo::new(
                        platform::connect_tcp_host(host, port, &servers).await?,
                    ))
                }
            }))
            .await?
    };
    Ok(NezhaServiceClient::new(channel))
}

enum SessionExit {
    Reload,
    Stop,
}

async fn session(
    runtime: Arc<Runtime>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<SessionExit> {
    let config = runtime.config().await;
    let mut reload_rx = runtime.subscribe();
    let mut client = connect(&config).await?;
    let mut monitor = Monitor::new();
    let host = monitor.host(&config);
    let receipt = time::timeout(
        Duration::from_secs(10),
        client.report_system_info2(authenticated(host, &config)?),
    )
    .await??;
    let mut dashboard_boot_time = receipt.get_ref().data;
    eprintln!(
        "connected to {} (service boot time {})",
        config.server,
        receipt.get_ref().data
    );

    let (task_tx, task_rx) = mpsc::channel::<TaskResult>(64);
    let (task_error_tx, mut task_error_rx) = mpsc::channel::<String>(1);
    let mut task_client = client.clone();
    let task_config = Arc::clone(&config);
    let task_runtime = Arc::clone(&runtime);
    let (task_stop_tx, mut task_stop_rx) = watch::channel(false);
    let task_worker =
        AbortOnDrop(tokio::spawn(async move {
            let mut session_tasks = tokio::task::JoinSet::new();
            let result: Result<()> = async {
            let mut stream = task_client
                .request_task(authenticated(ReceiverStream::new(task_rx), &task_config)?)
                .await
                .context("open RequestTask stream")?
                .into_inner();
            loop {
                let task = tokio::select! {
                    incoming = stream.message() => incoming.context("receive RequestTask message")?,
                    changed = task_stop_rx.changed() => {
                        if changed.is_err() || *task_stop_rx.borrow() {
                            return Ok(());
                        }
                        continue;
                    }
                };
                let Some(task) = task else {
                    anyhow::bail!("task stream ended");
                };
                while session_tasks.try_join_next().is_some() {}
                let sender = task_tx.clone();
                if matches!(task.r#type, 8 | 9 | 11 | 20) {
                    let config = Arc::clone(&task_config);
                    let client = task_client.clone();
                    session_tasks.spawn(async move {
                        let result = match task.r#type {
                            8 => terminal::run(task, &config, client).await,
                            9 => nat::run(task, &config, client).await,
                            11 => file_manager::run(task, &config, client).await,
                            _ => mcp_transfer::run(task, &config, client).await,
                        };
                        if let Err(error) = result {
                            eprintln!("IOStream session ended: {error:#}");
                        }
                    });
                } else if matches!(task.r#type, 13 | 14) {
                    let result = task_runtime.apply(task).await;
                    sender
                        .send(result)
                        .await
                        .context("send ApplyConfig result")?;
                } else if task.r#type == 6 {
                    let config = Arc::clone(&task_config);
                    session_tasks.spawn(async move {
                        if !config.disable_force_update {
                            if let Err(error) = updater::check(&config, true).await {
                                eprintln!("forced update failed: {error:#}");
                            }
                        }
                        let _ = sender.send(TaskResult {
                            id: task.id,
                            r#type: task.r#type,
                            ..Default::default()
                        }).await;
                    });
                } else if task.r#type == 12 && task_runtime.has_pending_reload().await {
                    sender
                        .send(TaskResult {
                            id: task.id,
                            r#type: task.r#type,
                            data: "another reload is in process".into(),
                            ..Default::default()
                        })
                        .await
                        .context("send ReportConfig rejection")?;
                } else {
                    let config = Arc::clone(&task_config);
                    session_tasks.spawn(async move {
                        if let Some(result) = tasks::run(task, (*config).clone()).await {
                            let _ = sender.send(result).await;
                        }
                    });
                }
            }
        }
        .await;
            session_tasks.shutdown().await;
            if let Err(error) = result {
                let _ = task_error_tx.send(format!("{error:#}")).await;
            }
        }));
    let outcome: Result<SessionExit> = async {
    let (state_tx, state_rx) = mpsc::channel(4);
    state_tx.send(monitor.state(&config)).await?;
    let mut receipts = client
        .report_system_state(authenticated(ReceiverStream::new(state_rx), &config)?)
        .await
        .context("open ReportSystemState stream")?
        .into_inner();
    time::timeout(Duration::from_secs(10), receipts.message())
        .await
        .context("initial ReportSystemState receipt timed out")?
        .context("receive initial ReportSystemState receipt")?
        .context("initial state receipt stream ended")?;
    if config.debug {
        eprintln!("initial state receipt received");
    }
    let mut ticker = time::interval(Duration::from_secs(config.report_delay as u64));
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut last_host = time::Instant::now();
    let geoip_period = Duration::from_secs(config.ip_report_period as u64);
    let mut geoip_due = time::Instant::now();
    let mut geoip_pending = false;
    let mut geoip_failures = 0;
    let mut geoip_reported = false;
    let mut last_geoip_query: Option<String> = None;
    let (geoip_tx, mut geoip_rx) = mpsc::channel(1);
    let mut geoip_job: Option<AbortOnDrop> = None;

    loop {
        tokio::select! {
            _ = time::sleep_until(geoip_due), if !geoip_pending => {
                geoip_pending = true;
                let config = Arc::clone(&config);
                let sender = geoip_tx.clone();
                geoip_job = Some(AbortOnDrop(tokio::spawn(async move {
                    let _ = sender.send(geoip::fetch(&config).await).await;
                })));
            }
            Some(fetched) = geoip_rx.recv() => {
                geoip_pending = false;
                geoip_job.take();
                let report = match fetched {
                    Some(report) => {
                        geoip_failures = 0;
                        report
                    }
                    None => {
                        geoip_failures += 1;
                        if geoip_failures <= 3 {
                            if config.debug { eprintln!("GeoIP lookup failed (attempt {geoip_failures}); retrying"); }
                            geoip_due = time::Instant::now() + Duration::from_secs(3);
                            continue;
                        }
                        if config.debug { eprintln!("GeoIP lookup failed; using dashboard connection IP"); }
                        geoip_failures = 0;
                        geoip::fallback()
                    }
                };
                let query_ip = geoip::query_ip(&report).to_owned();
                if geoip_reported && last_geoip_query.as_deref() == Some(&query_ip) {
                    geoip_due = time::Instant::now() + geoip_period;
                    continue;
                }
                let response = time::timeout(Duration::from_secs(10),
                    client.report_geo_ip(authenticated(report, &config)?)).await;
                match response {
                    Ok(Ok(receipt)) => {
                        geoip_reported = true;
                        last_geoip_query = Some(query_ip);
                        dashboard_boot_time = receipt.get_ref().dashboard_boot_time;
                        geoip_due = time::Instant::now() + geoip_period;
                        if config.debug {
                            eprintln!("GeoIP reported, country={}", receipt.get_ref().country_code);
                        }
                    }
                    Ok(Err(error)) => {
                        eprintln!("GeoIP report failed: {error}");
                        geoip_due = time::Instant::now() + Duration::from_secs(3);
                    }
                    Err(_) => {
                        eprintln!("GeoIP report timed out");
                        geoip_due = time::Instant::now() + Duration::from_secs(3);
                    }
                }
            }
            _ = ticker.tick() => {
                let state = monitor.state(&config);
                state_tx.send(state).await.context("send ReportSystemState message")?;
                time::timeout(Duration::from_secs(10), receipts.message()).await
                    .context("ReportSystemState receipt timed out")?
                    .context("receive ReportSystemState receipt")?
                    .context("state receipt stream ended")?;
                if config.debug { eprintln!("state receipt received"); }
                if last_host.elapsed() >= Duration::from_secs(600) {
                    let receipt = client.report_system_info2(authenticated(monitor.host(&config), &config)?).await?;
                    if receipt.get_ref().data != dashboard_boot_time {
                        dashboard_boot_time = receipt.get_ref().data;
                        geoip_reported = false;
                        geoip_due = time::Instant::now();
                    }
                    last_host = time::Instant::now();
                }
            }
            error = task_error_rx.recv() => {
                anyhow::bail!("{}", error.unwrap_or_else(|| "task worker ended".into()));
            }
            result = reload_rx.changed() => {
                result.context("reload notification closed")?;
                return Ok(SessionExit::Reload);
            }
            _ = tokio::signal::ctrl_c() => return Ok(SessionExit::Stop),
            result = platform::wait_terminate() => {
                result?;
                return Ok(SessionExit::Stop);
            },
            result = shutdown.changed() => {
                result.context("shutdown notification closed")?;
                if *shutdown.borrow() { return Ok(SessionExit::Stop); }
            }
        }
    }
    }.await;
    let _ = task_stop_tx.send(true);
    let mut task_worker = task_worker;
    if time::timeout(Duration::from_secs(5), &mut task_worker.0)
        .await
        .is_err()
    {
        task_worker.0.abort();
    }
    outcome
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Edit)) {
        return edit::run(&args);
    }
    if let Some(Command::Service { action, user }) = args.command.as_ref() {
        return service::control(&args, *action, *user);
    }
    #[cfg(windows)]
    if service::maybe_run(&args)? {
        return Ok(());
    }
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    run_agent(args, shutdown_rx).await
}

async fn run_agent(args: Args, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let runtime = Runtime::new(AgentConfig::load(&args)?);
    let startup_config = runtime.config().await;
    if !startup_config.disable_auto_update && !startup_config.rust_update_manifest_url.is_empty() {
        if let Err(error) = updater::check(&startup_config, false).await {
            eprintln!("startup update check failed: {error:#}");
        }
    }
    let periodic_runtime = Arc::clone(&runtime);
    let _periodic_update = AbortOnDrop(tokio::spawn(async move {
        loop {
            let config = periodic_runtime.config().await;
            let minutes = if config.self_update_period > 0 {
                config.self_update_period as u64
            } else {
                1440 + (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    % 1440)
            };
            time::sleep(Duration::from_secs(minutes * 60)).await;
            let config = periodic_runtime.config().await;
            if !config.disable_auto_update && !config.rust_update_manifest_url.is_empty() {
                if let Err(error) = updater::check(&config, false).await {
                    eprintln!("periodic update check failed: {error:#}");
                }
            }
        }
    }));
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let outcome = tokio::select! {
            result = session(Arc::clone(&runtime), &mut shutdown) => result,
            _ = tokio::signal::ctrl_c() => return Ok(()),
            result = platform::wait_terminate() => { result?; return Ok(()); }
        };
        match outcome {
            Ok(SessionExit::Reload) => continue,
            Ok(SessionExit::Stop) => return Ok(()),
            Err(error) => {
                eprintln!(
                    "session ended at {:?}: {error:#}; reconnecting in 5 seconds",
                    std::time::SystemTime::now()
                );
                tokio::select! {
                    _ = time::sleep(Duration::from_secs(5)) => {},
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                    result = platform::wait_terminate() => { result?; return Ok(()); }
                    result = shutdown.changed() => {
                        result.context("shutdown notification closed")?;
                        if *shutdown.borrow() { return Ok(()); }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::proto::{Host, State};
    use super::AbortOnDrop;
    use prost::Message;
    use std::time::Duration;

    #[test]
    fn current_wire_field_numbers_match_go_agent() {
        let state = State {
            mem_used: 1,
            ..Default::default()
        };
        assert_eq!(state.encode_to_vec(), [0x10, 0x01]);
        let host = Host {
            version: "v".into(),
            ..Default::default()
        };
        assert_eq!(host.encode_to_vec(), [0x52, 0x01, b'v']);
    }

    #[tokio::test]
    async fn pending_iostream_open_is_cancelled_with_its_session() {
        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                if let Some(signal) = self.0.take() {
                    let _ = signal.send(());
                }
            }
        }

        let (signal, dropped) = tokio::sync::oneshot::channel();
        let task = AbortOnDrop(tokio::spawn(async move {
            let _signal = DropSignal(Some(signal));
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;
        drop(task);
        tokio::time::timeout(Duration::from_secs(2), dropped)
            .await
            .unwrap()
            .unwrap();
    }
}
