use crate::{
    config::AgentConfig,
    proto::{Task, TaskResult},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{watch, Mutex, RwLock},
    task::JoinHandle,
    time,
};

struct PendingReload {
    generation: u64,
    transfer: bool,
    timer: JoinHandle<()>,
}

pub struct Runtime {
    current: RwLock<AgentConfig>,
    pending: Mutex<Option<PendingReload>>,
    reload: watch::Sender<u64>,
}

impl Runtime {
    pub fn new(config: AgentConfig) -> Arc<Self> {
        let (reload, _) = watch::channel(0);
        Arc::new(Self {
            current: RwLock::new(config),
            pending: Mutex::new(None),
            reload,
        })
    }

    pub async fn config(&self) -> Arc<AgentConfig> {
        Arc::new(self.current.read().await.clone())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.reload.subscribe()
    }

    pub async fn has_pending_reload(&self) -> bool {
        self.pending.lock().await.is_some()
    }

    pub async fn apply(self: &Arc<Self>, task: Task) -> TaskResult {
        let mut result = TaskResult {
            id: task.id,
            r#type: task.r#type,
            ..Default::default()
        };
        let is_transfer = task.r#type == 14;
        let mut pending = self.pending.lock().await;
        let baseline = self.current.read().await.clone();
        if baseline.disable_command_execute {
            result.data =
                "This agent has disabled command execution (DisableCommandExecute)".into();
            return result;
        }
        let mut merged = match serde_json::to_value(&baseline) {
            Ok(value) => value,
            Err(error) => {
                result.data = error.to_string();
                return result;
            }
        };
        let payload: serde_json::Value = match serde_json::from_str(&task.data) {
            Ok(value) => value,
            Err(error) => {
                result.data = error.to_string();
                return result;
            }
        };
        let Some(fields) = payload.as_object() else {
            result.data = "configuration payload must be a JSON object".into();
            return result;
        };
        let target = merged
            .as_object_mut()
            .expect("AgentConfig serializes as object");
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
        let mut next: AgentConfig = match serde_json::from_value(merged) {
            Ok(config) => config,
            Err(error) => {
                result.data = error.to_string();
                return result;
            }
        };
        next.config_path = baseline.config_path.clone();
        if let Err(error) = next.validate(true) {
            result.data = error.to_string();
            return result;
        }
        if is_transfer {
            let secret = next.client_secret.as_bytes();
            if secret.len() != 32 || !secret.iter().all(u8::is_ascii_alphanumeric) {
                result.data =
                    "rejected client_secret rotation: expected 32 alphanumeric bytes".into();
                return result;
            }
            if !next.tls || next.insecure_tls {
                result.data = "ServerTransferApply rejected: rotated secret cannot be delivered over plaintext or InsecureTLS".into();
                return result;
            }
        } else {
            if next.client_secret != baseline.client_secret {
                result.data = "ApplyConfig rejected: client_secret rotation must use TaskTypeServerTransferApply".into();
                return result;
            }
            if pending.as_ref().is_some_and(|reload| reload.transfer) {
                result.data = "transfer reload in progress".into();
                return result;
            }
        }

        let generation = pending.as_ref().map_or(1, |reload| reload.generation + 1);
        if let Some(old) = pending.take() {
            old.timer.abort();
        }
        let runtime = Arc::clone(self);
        let timer = tokio::spawn(async move {
            time::sleep(Duration::from_secs(10)).await;
            runtime.commit(generation, next).await;
        });
        *pending = Some(PendingReload {
            generation,
            transfer: is_transfer,
            timer,
        });
        result.successful = true;
        result
    }

    async fn commit(&self, generation: u64, config: AgentConfig) {
        let mut pending = self.pending.lock().await;
        if pending.as_ref().map(|reload| reload.generation) != Some(generation) {
            return;
        }
        if let Err(error) = config.save() {
            eprintln!("save new config failed: {error:#}");
            return;
        }
        *self.current.write().await = config;
        pending.take();
        let _ = self.reload.send(generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AgentConfig {
        AgentConfig {
            server: "127.0.0.1:18539".into(),
            client_secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            uuid: "00000000-0000-0000-0000-000000000001".into(),
            tls: true,
            config_path: std::env::temp_dir().join("nezha-rust-runtime-test.yml"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn generic_apply_rejects_secret_rotation() {
        let runtime = Runtime::new(config());
        let result = runtime
            .apply(Task {
                id: 1,
                r#type: 13,
                data: r#"{"client_secret":"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"}"#.into(),
            })
            .await;
        assert!(!result.successful);
        assert!(result.data.contains("rotation"));
        assert!(!runtime.has_pending_reload().await);
    }

    #[tokio::test]
    async fn transfer_requires_verified_tls_and_valid_secret() {
        let runtime = Runtime::new(config());
        let result = runtime
            .apply(Task {
                id: 2,
                r#type: 14,
                data: r#"{"client_secret":"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","tls":false}"#.into(),
            })
            .await;
        assert!(!result.successful);
        assert!(result.data.contains("plaintext"));
    }
}
