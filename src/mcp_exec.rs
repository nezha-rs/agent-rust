use crate::{
    config::AgentConfig,
    proto::{Task, TaskResult},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

#[derive(Default, Deserialize)]
#[serde(default)]
struct ExecRequest {
    cmd: String,
    args: Vec<String>,
    cwd: String,
    env: HashMap<String, String>,
    timeout_seconds: u32,
    stdin: String,
    max_output_bytes: u32,
}

#[derive(Default, Serialize)]
struct ExecResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u128,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stdout_truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stderr_truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    timed_out: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, max: usize) -> (String, bool) {
    let mut output = Vec::with_capacity(max.min(8192));
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let copy = count.min(max.saturating_sub(output.len()));
                output.extend_from_slice(&chunk[..copy]);
                if copy < count {
                    truncated = true;
                }
            }
        }
    }
    (String::from_utf8_lossy(&output).to_string(), truncated)
}

fn reply(task: Task, data: ExecResult) -> TaskResult {
    TaskResult {
        id: task.id,
        r#type: task.r#type,
        data: serde_json::to_string(&data)
            .unwrap_or_else(|_| "{\"error\":\"marshal failed\"}".into()),
        successful: true,
        ..Default::default()
    }
}

pub async fn run(task: Task, config: &AgentConfig) -> TaskResult {
    if config.disable_command_execute {
        return reply(
            task,
            ExecResult {
                error: "agent disabled command execution".into(),
                ..Default::default()
            },
        );
    }
    let request: ExecRequest = match serde_json::from_str(&task.data) {
        Ok(request) => request,
        Err(error) => {
            return TaskResult {
                id: task.id,
                r#type: task.r#type,
                data: format!("invalid exec request: {error}"),
                ..Default::default()
            }
        }
    };
    if request.cmd.trim().is_empty() {
        return reply(
            task,
            ExecResult {
                error: "cmd required".into(),
                ..Default::default()
            },
        );
    }
    let seconds = if request.timeout_seconds == 0 {
        30
    } else {
        request.timeout_seconds.min(300)
    };
    let max_output = if request.max_output_bytes == 0 {
        64 * 1024
    } else {
        request.max_output_bytes.min(1024 * 1024)
    } as usize;
    let mut process = Command::new(&request.cmd);
    process
        .args(&request.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !request.cwd.is_empty() {
        process.current_dir(&request.cwd);
    }
    process.env_clear();
    for key in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TZ",
        "TERM",
        "SystemRoot",
        "windir",
        "TEMP",
        "TMP",
        "PATHEXT",
        "ComSpec",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SystemDrive",
        "ProgramFiles",
    ] {
        if let Some(value) = std::env::var_os(key) {
            process.env(key, value);
        }
    }
    process.envs(&request.env);
    crate::platform::configure_command(&mut process);
    let start = Instant::now();
    let mut process_guard = match crate::platform::new_process_guard() {
        Ok(job) => job,
        Err(error) => {
            return reply(
                task,
                ExecResult {
                    exit_code: -1,
                    error: error.to_string(),
                    ..Default::default()
                },
            )
        }
    };
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return reply(
                task,
                ExecResult {
                    exit_code: -1,
                    error: error.to_string(),
                    ..Default::default()
                },
            )
        }
    };
    if let Err(error) = crate::platform::attach_process_guard(&mut process_guard, child.id()) {
        process_guard.terminate();
        let _ = child.kill().await;
        return reply(
            task,
            ExecResult {
                exit_code: -1,
                error: error.to_string(),
                ..Default::default()
            },
        );
    }
    let stdout = tokio::spawn(read_bounded(
        child.stdout.take().expect("piped stdout"),
        max_output,
    ));
    let stderr = tokio::spawn(read_bounded(
        child.stderr.take().expect("piped stderr"),
        max_output,
    ));
    if let Some(mut stdin) = child.stdin.take() {
        let input = request.stdin;
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        });
    }
    let waited = timeout(Duration::from_secs(seconds.into()), child.wait()).await;
    let (status, timed_out, error) = match waited {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false, String::new()),
        Ok(Err(error)) => (-1, false, error.to_string()),
        Err(_) => {
            process_guard.terminate();
            let _ = child.kill().await;
            (-1, true, String::new())
        }
    };
    process_guard.terminate();
    let mut stdout = stdout;
    let mut stderr = stderr;
    let stdout_data = match timeout(Duration::from_millis(500), &mut stdout).await {
        Ok(Ok(data)) => data,
        _ => {
            stdout.abort();
            (String::new(), false)
        }
    };
    let stderr_data = match timeout(Duration::from_millis(500), &mut stderr).await {
        Ok(Ok(data)) => data,
        _ => {
            stderr.abort();
            (String::new(), false)
        }
    };
    reply(
        task,
        ExecResult {
            exit_code: status,
            stdout: stdout_data.0,
            stderr: stderr_data.0,
            duration_ms: start.elapsed().as_millis(),
            stdout_truncated: stdout_data.1,
            stderr_truncated: stderr_data.1,
            timed_out,
            error,
        },
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_exact_stdout_and_structured_failure() {
        let task = Task {
            id: 7,
            r#type: 15,
            data: r#"{"cmd":"/bin/sh","args":["-c","printf compat-exec"]}"#.into(),
        };
        let result = run(task, &AgentConfig::default()).await;
        assert!(result.successful);
        let response: serde_json::Value = serde_json::from_str(&result.data).unwrap();
        assert_eq!(response["exit_code"], 0);
        assert_eq!(response["stdout"], "compat-exec");
    }

    #[tokio::test]
    async fn truncates_output_at_requested_limit() {
        let task = Task {
            id: 8,
            r#type: 15,
            data: r#"{"cmd":"/bin/sh","args":["-c","printf 123456789"],"max_output_bytes":4}"#
                .into(),
        };
        let result = run(task, &AgentConfig::default()).await;
        let response: serde_json::Value = serde_json::from_str(&result.data).unwrap();
        assert_eq!(response["stdout"], "1234");
        assert_eq!(response["stdout_truncated"], true);
    }

    #[tokio::test]
    async fn times_out_process_group() {
        let task = Task {
            id: 9,
            r#type: 15,
            data: r#"{"cmd":"/bin/sh","args":["-c","sleep 5"],"timeout_seconds":1}"#.into(),
        };
        let started = Instant::now();
        let result = run(task, &AgentConfig::default()).await;
        let response: serde_json::Value = serde_json::from_str(&result.data).unwrap();
        assert_eq!(response["timed_out"], true);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
