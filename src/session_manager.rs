use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tokio::sync::broadcast::Receiver;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::bridge::{BridgeOptions, CodexAppServerBridge};
use crate::config::AppConfig;
use crate::error::AppError;
use crate::models::{BridgeEvent, BridgeStateSnapshot, SessionInfo};

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionManagerInner>,
}

struct SessionManagerInner {
    config: AppConfig,
    started_at: Instant,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    create_lock: Mutex<()>,
}

struct Session {
    id: String,
    backend: SessionBackend,
    metadata: Arc<SessionMetadata>,
}

enum SessionBackend {
    Embedded { bridge: CodexAppServerBridge },
}

struct SessionMetadata {
    created_at: DateTime<Utc>,
    last_access_at: RwLock<DateTime<Utc>>,
    expires_at: RwLock<DateTime<Utc>>,
}

impl SessionManager {
    pub fn new(config: AppConfig) -> Self {
        let manager = Self {
            inner: Arc::new(SessionManagerInner {
                config,
                started_at: Instant::now(),
                sessions: RwLock::new(HashMap::new()),
                create_lock: Mutex::new(()),
            }),
        };

        manager.spawn_sweeper();
        manager
    }

    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    pub fn count(&self) -> usize {
        self.inner.sessions.read().unwrap().len()
    }

    pub async fn create_session(
        &self,
        model: Option<String>,
        resume_thread_id: Option<String>,
    ) -> Result<(String, SessionInfo, BridgeStateSnapshot), AppError> {
        let _guard = self.inner.create_lock.lock().await;
        self.sweep_expired_sessions().await;

        if self.count() >= self.inner.config.max_sessions {
            warn!(
                active_sessions = self.count(),
                max_sessions = self.inner.config.max_sessions,
                "maximum concurrent sessions reached"
            );
            return Err(AppError::service_unavailable(format!(
                "Maximum concurrent sessions reached ({})",
                self.inner.config.max_sessions
            )));
        }

        let id = Uuid::new_v4().to_string();
        info!(
            session_id = %id,
            model = model.as_deref().unwrap_or("-"),
            resume_thread_id = resume_thread_id.as_deref().unwrap_or("-"),
            "allocating session"
        );
        let metadata = Arc::new(SessionMetadata::new(self.inner.config.session_ttl));
        let backend = self
            .create_embedded_backend(model, resume_thread_id, &metadata)
            .await?;

        let session = Arc::new(Session {
            id: id.clone(),
            backend,
            metadata,
        });
        let info = session.info();
        let state = session.state();

        self.inner
            .sessions
            .write()
            .unwrap()
            .insert(id.clone(), session);
        info!("session created {}", id);

        Ok((id, info, state))
    }

    pub fn get_state(&self, session_id: &str) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        Ok(session.state())
    }

    pub fn get_session_info(&self, session_id: &str) -> Result<SessionInfo, AppError> {
        let session = self.require_session(session_id)?;
        Ok(session.info())
    }

    pub fn subscribe(
        &self,
        session_id: &str,
    ) -> Result<(SessionInfo, BridgeStateSnapshot, Receiver<BridgeEvent>), AppError> {
        let session = self.require_session(session_id)?;
        info!(session_id = %session_id, "subscribing to session events");
        let receiver = match &session.backend {
            SessionBackend::Embedded { bridge } => bridge.subscribe(),
        };
        Ok((session.info(), session.state(), receiver))
    }

    pub async fn send_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        info!(
            session_id = %session_id,
            prompt_len = prompt.chars().count(),
            "forwarding prompt to bridge"
        );
        session.send_prompt(prompt).await
    }

    pub async fn interrupt_turn(&self, session_id: &str) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        info!(session_id = %session_id, "forwarding interrupt to bridge");
        session.interrupt_turn().await
    }

    pub async fn close_session(&self, session_id: &str, reason: &str) -> Result<bool, AppError> {
        let session = self.inner.sessions.write().unwrap().remove(session_id);
        let Some(session) = session else {
            return Ok(false);
        };

        session.close(reason).await?;
        info!("session closed {} ({reason})", session_id);
        Ok(true)
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        let ids = self
            .inner
            .sessions
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        for id in ids {
            let _ = self.close_session(&id, "shutdown").await;
        }

        Ok(())
    }

    async fn create_embedded_backend(
        &self,
        model: Option<String>,
        resume_thread_id: Option<String>,
        metadata: &Arc<SessionMetadata>,
    ) -> Result<SessionBackend, AppError> {
        let bridge = CodexAppServerBridge::new(BridgeOptions {
            cwd: self.inner.config.bridge_cwd.clone(),
            codex_bin: self.inner.config.codex_bin.clone(),
            debug: self.inner.config.debug,
            client_info: self.inner.config.client_info.clone(),
            default_model: self.inner.config.default_model.clone(),
            activity_touch: metadata.touch_callback(self.inner.config.session_ttl),
        });

        bridge.start_without_thread().await?;
        let init_result = if let Some(thread_id) = resume_thread_id {
            bridge.resume_thread(&thread_id).await.map(|_| ())
        } else {
            bridge.start_new_thread(model).await.map(|_| ())
        };
        if let Err(error) = init_result {
            let _ = bridge.stop().await;
            return Err(error);
        }

        Ok(SessionBackend::Embedded { bridge })
    }

    async fn sweep_expired_sessions(&self) {
        let now = Utc::now();
        let expired_ids = self
            .inner
            .sessions
            .read()
            .unwrap()
            .iter()
            .filter_map(|(id, session)| {
                if *session.metadata.expires_at.read().unwrap() <= now {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for id in expired_ids {
            if let Err(error) = self.close_session(&id, "expired").await {
                error!("failed to close expired session {id}: {error}");
            }
        }
    }

    fn require_session(&self, session_id: &str) -> Result<Arc<Session>, AppError> {
        let session = self
            .inner
            .sessions
            .read()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("Unknown session: {session_id}")))?;
        session.metadata.touch(self.inner.config.session_ttl);
        Ok(session)
    }

    fn spawn_sweeper(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            info!(
                session_sweep_interval_ms =
                    manager.inner.config.session_sweep_interval.as_millis() as u64,
                "session sweeper started"
            );
            loop {
                tokio::time::sleep(manager.inner.config.session_sweep_interval).await;
                manager.sweep_expired_sessions().await;
            }
        });
    }
}

impl Session {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            created_at: self.metadata.created_at,
            last_access_at: *self.metadata.last_access_at.read().unwrap(),
            expires_at: *self.metadata.expires_at.read().unwrap(),
        }
    }

    fn state(&self) -> BridgeStateSnapshot {
        match &self.backend {
            SessionBackend::Embedded { bridge } => bridge.get_state(),
        }
    }

    async fn send_prompt(&self, prompt: &str) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Embedded { bridge } => {
                bridge.send_prompt(prompt).await?;
                Ok(bridge.get_state())
            }
        }
    }

    async fn interrupt_turn(&self) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Embedded { bridge } => {
                bridge.interrupt_turn().await?;
                Ok(bridge.get_state())
            }
        }
    }

    async fn close(&self, reason: &str) -> Result<(), AppError> {
        match &self.backend {
            SessionBackend::Embedded { bridge } => {
                bridge.broadcast_session_closed(&self.id, reason);
                bridge.stop().await
            }
        }
    }
}

impl SessionMetadata {
    fn new(ttl: Duration) -> Self {
        let now = Utc::now();
        Self {
            created_at: now,
            last_access_at: RwLock::new(now),
            expires_at: RwLock::new(now + chrono::Duration::from_std(ttl).unwrap_or_default()),
        }
    }

    fn touch(&self, ttl: Duration) {
        let now = Utc::now();
        *self.last_access_at.write().unwrap() = now;
        *self.expires_at.write().unwrap() =
            now + chrono::Duration::from_std(ttl).unwrap_or_default();
    }

    fn touch_callback(self: &Arc<Self>, ttl: Duration) -> Arc<dyn Fn() + Send + Sync> {
        let metadata = Arc::clone(self);
        Arc::new(move || metadata.touch(ttl))
    }
}
