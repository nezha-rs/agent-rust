use crate::{
    config::AgentConfig,
    mcp_exec, mcp_fs,
    proto::{Task, TaskResult},
};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, time::timeout};

pub async fn run(task: Task, config: AgentConfig) -> Option<TaskResult> {
    if task.r#type == 15 {
        return Some(mcp_exec::run(task, &config).await);
    }
    if (16..=19).contains(&task.r#type) {
        return Some(mcp_fs::run(task, &config).await);
    }
    let mut result = TaskResult {
        id: task.id,
        r#type: task.r#type,
        ..Default::default()
    };
    match task.r#type {
        1 => http_get(&task.data, &config, &mut result).await,
        2 => icmp_ping(&task.data, &config, &mut result).await,
        3 => tcp_ping(&task.data, &config, &mut result).await,
        4 => command(&task.data, &config, &mut result).await,
        7 => {}
        12 => {
            if config.disable_command_execute {
                result.data = "This agent has disabled command execution".into();
            } else {
                result.data = serde_json::to_string(&config).unwrap_or_default();
                result.successful = true;
            }
        }
        _ => {
            eprintln!("task type {} is not implemented", task.r#type);
            return None;
        }
    }
    Some(result)
}

async fn tcp_ping(address: &str, config: &AgentConfig, result: &mut TaskResult) {
    if config.disable_send_query {
        result.data = "This server has disabled query sending".into();
        return;
    }
    let (host, port) = match crate::endpoint::split_host_port(address) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            result.data = error;
            return;
        }
    };
    let target = match crate::platform::resolve_first_ip(host, &config.dns).await {
        Ok(ip) => std::net::SocketAddr::new(ip, port),
        Err(error) => {
            result.data = error.to_string();
            return;
        }
    };
    let start = Instant::now();
    match timeout(Duration::from_secs(10), TcpStream::connect(target)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            result.delay = start.elapsed().as_secs_f32() * 1000.0;
            result.successful = true;
        }
        Ok(Err(error)) => result.data = error.to_string(),
        Err(_) => result.data = "connection timed out".into(),
    }
}

async fn http_get(url: &str, config: &AgentConfig, result: &mut TaskResult) {
    if config.disable_send_query {
        result.data = "This server has disabled query sending".into();
        return;
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        result.data = "invalid URL: only http and https schemes are supported".into();
        return;
    }
    let start = Instant::now();
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .tls_info(true)
        .timeout(Duration::from_secs(30))
        .user_agent("nezha-agent/1.0")
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8"
                    .parse()
                    .expect("static Accept header is valid"),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                "en,zh-CN;q=0.9,zh;q=0.8"
                    .parse()
                    .expect("static Accept-Language header is valid"),
            );
            headers
        });
    let client = match crate::platform::http_client_builder(builder, &config.dns)
        .and_then(|builder| builder.build().map_err(std::io::Error::other))
    {
        Ok(client) => client,
        Err(error) => {
            result.data = error.to_string();
            return;
        }
    };
    match client.get(url).send().await {
        Ok(mut response) => {
            let status = response.status();
            let certificate = response
                .extensions()
                .get::<reqwest::tls::TlsInfo>()
                .and_then(reqwest::tls::TlsInfo::peer_certificate)
                .and_then(certificate_status);
            let mut body_error = None;
            loop {
                match response.chunk().await {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        body_error = Some(error);
                        break;
                    }
                }
            }
            match body_error {
                None => {
                    result.delay = start.elapsed().as_secs_f32() * 1000.0;
                    if (200..400).contains(&status.as_u16()) {
                        result.data = certificate.unwrap_or_default();
                        result.successful = true;
                    } else {
                        result.data = format!("\n\u{5e94}\u{7528}\u{9519}\u{8bef}: {status}");
                    }
                }
                Some(error) => result.data = error.to_string(),
            }
        }
        Err(error) => result.data = error.to_string(),
    }
}

fn certificate_status(der: &[u8]) -> Option<String> {
    let (_, certificate) = x509_parser::parse_x509_certificate(der).ok()?;
    let issuer = certificate
        .issuer()
        .iter_common_name()
        .next()
        .and_then(|name| name.as_str().ok())
        .unwrap_or_default();
    let expiry = certificate.validity().not_after.to_datetime();
    Some(format!(
        "{}|{:04}-{:02}-{:02} {:02}:{:02}:{:02} +0000 UTC",
        issuer,
        expiry.year(),
        u8::from(expiry.month()),
        expiry.day(),
        expiry.hour(),
        expiry.minute(),
        expiry.second()
    ))
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn http_monitor_sends_upstream_headers_and_does_not_follow_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let count = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/unreachable\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..count]).to_ascii_lowercase()
        });
        let mut result = TaskResult::default();
        http_get(
            &format!("http://{address}/"),
            &AgentConfig::default(),
            &mut result,
        )
        .await;
        let request = server.await.unwrap();
        assert!(request.contains("user-agent: nezha-agent/1.0"));
        assert!(request.contains("accept-language: en,zh-cn;q=0.9,zh;q=0.8"));
        assert!(result.successful, "{}", result.data);
    }
}

async fn icmp_ping(host: &str, config: &AgentConfig, result: &mut TaskResult) {
    if config.disable_send_query {
        result.data = "This server has disabled query sending".into();
        return;
    }
    let target = match crate::platform::resolve_first_ip(host, &config.dns).await {
        Ok(address) => address,
        Err(error) => {
            result.data = error.to_string();
            return;
        }
    };
    match crate::platform::icmp_ping(target).await {
        Ok(Some(delay)) => {
            result.delay = delay;
            result.successful = true;
        }
        Ok(None) => result.data = "pockets recv 0".into(),
        Err(error) => result.data = error,
    }
}

async fn command(text: &str, config: &AgentConfig, result: &mut TaskResult) {
    command_with_timeout(text, config, result, Duration::from_secs(7200)).await;
}

async fn command_with_timeout(
    text: &str,
    config: &AgentConfig,
    result: &mut TaskResult,
    limit: Duration,
) {
    if config.disable_command_execute {
        result.data =
            "\u{6b64} Agent \u{5df2}\u{7981}\u{6b62}\u{547d}\u{4ee4}\u{6267}\u{884c}".into();
        return;
    }
    let start = Instant::now();
    let mut process = crate::platform::shell_command(text);
    process
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::platform::configure_command(&mut process);
    let mut process_guard = match crate::platform::new_process_guard() {
        Ok(job) => job,
        Err(error) => {
            result.data = error.to_string();
            result.delay = start.elapsed().as_secs_f32();
            return;
        }
    };
    #[allow(unused_mut)]
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            result.data = error.to_string();
            result.delay = start.elapsed().as_secs_f32();
            return;
        }
    };
    if let Err(error) = crate::platform::attach_process_guard(&mut process_guard, child.id()) {
        process_guard.terminate();
        let _ = child.kill().await;
        result.data = error.to_string();
        result.delay = start.elapsed().as_secs_f32();
        return;
    }
    let output = timeout(limit, child.wait_with_output()).await;
    match output {
        Ok(Ok(output)) => {
            result.data = String::from_utf8_lossy(&output.stdout).to_string();
            if !output.status.success() {
                let failure = output.status.code().map_or_else(
                    || output.status.to_string(),
                    |code| format!("exit status {code}"),
                );
                result.data.push_str(&format!("\n{failure}"));
            }
            result.successful = output.status.success();
        }
        Ok(Err(error)) => result.data = error.to_string(),
        Err(_) => {
            process_guard.terminate();
            result.data = "task execution timed out".into();
        }
    }
    result.delay = start.elapsed().as_secs_f32();
}

#[cfg(all(test, unix))]
mod command_tests {
    use super::*;
    use prost::Message;

    #[tokio::test]
    async fn timeout_reaps_background_child() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("orphan");
        let ready = directory.path().join("started");
        let escaped = marker.display().to_string().replace('\'', "'\\''");
        let script = format!(
            "printf ready > '{}'; (sleep 1; printf orphan > '{escaped}') & sleep 30",
            ready.display()
        );
        let mut result = TaskResult::default();
        command_with_timeout(
            &script,
            &AgentConfig::default(),
            &mut result,
            Duration::from_millis(150),
        )
        .await;
        assert!(!result.successful);
        assert_eq!(result.data, "task execution timed out");
        assert!(ready.exists(), "command did not start before timeout");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !marker.exists(),
            "background child survived command timeout"
        );
    }

    #[tokio::test]
    async fn failure_result_matches_go_stdout_and_status() {
        let mut result = TaskResult::default();
        command(
            "printf compat; printf ignored >&2; exit 7",
            &AgentConfig::default(),
            &mut result,
        )
        .await;
        assert!(!result.successful);
        let wire = result.encode_to_vec();
        let decoded = TaskResult::decode(wire.as_slice()).unwrap();
        assert_eq!(decoded.data, "compat\nexit status 7");
    }

    #[tokio::test]
    async fn cancellation_reaps_background_child() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("cancelled-orphan");
        let ready = directory.path().join("started");
        let escaped = marker.display().to_string().replace('\'', "'\\''");
        let script = format!(
            "printf ready > '{}'; (sleep 1; printf orphan > '{escaped}') & sleep 30",
            ready.display()
        );
        let job = tokio::spawn(async move {
            let mut result = TaskResult::default();
            command_with_timeout(
                &script,
                &AgentConfig::default(),
                &mut result,
                Duration::from_secs(5),
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        job.abort();
        let _ = job.await;
        assert!(ready.exists(), "command did not start before cancellation");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !marker.exists(),
            "background child survived task cancellation"
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_ping_average_from_summary() {
        let output = "Reply from 127.0.0.1: bytes=32 time<1ms TTL=128\r\n\
            Minimum = 0ms, Maximum = 1ms, Average = 0ms\r\n";
        assert_eq!(crate::platform::ping_average(output), Some(0.0));
    }

    #[tokio::test]
    async fn timeout_reaps_windows_job_tree() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("windows-orphan.txt");
        let ready = directory.path().join("windows-started.txt");
        let marker = marker.to_string_lossy().replace('"', "\\\"");
        let ready = ready.to_string_lossy().replace('"', "\\\"");
        let script = format!(
            "(echo ready>{ready}) & start \"\" /b cmd /c \"ping -n 3 127.0.0.1 >NUL & echo orphan>{marker}\" & ping -n 30 127.0.0.1 >NUL"
        );
        let mut result = TaskResult::default();
        command_with_timeout(
            &script,
            &AgentConfig::default(),
            &mut result,
            Duration::from_millis(250),
        )
        .await;
        assert!(!result.successful);
        assert_eq!(result.data, "task execution timed out");
        assert!(std::path::Path::new(&ready).exists());
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!std::path::Path::new(&marker).exists());
    }
}

#[cfg(test)]
mod https_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires local HTTPS fixture and SSL_CERT_FILE"]
    async fn https_monitor_reports_issuer_and_expiry() {
        let url = std::env::var("NZ_TEST_HTTPS_MONITOR_URL").unwrap();
        let mut result = TaskResult::default();
        http_get(&url, &AgentConfig::default(), &mut result).await;
        assert!(result.successful, "{}", result.data);
        assert!(
            result.data.starts_with("NezhaLocalTestCA|"),
            "{}",
            result.data
        );
        assert!(result.data.ends_with(" +0000 UTC"), "{}", result.data);
    }
}
