use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::broadcast::Receiver;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::bridge::{BridgeOptions, CodexAppServerBridge};
use crate::config::{AppConfig, AuthConfig, SessionRuntimeMode};
use crate::devbox::{DevboxRuntime, DevboxRuntimeManager};
use crate::error::AppError;
use crate::models::{BridgeEvent, BridgeStateSnapshot, SessionInfo};
use crate::remote_gateway::{RemoteGatewayClient, RemoteSession};

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionManagerInner>,
}

struct SessionManagerInner {
    config: AppConfig,
    devbox_runtime_manager: Option<DevboxRuntimeManager>,
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
    Local { bridge: CodexAppServerBridge },
    RemoteDevbox(Box<RemoteDevboxBackend>),
}

struct RemoteDevboxBackend {
    runtime: DevboxRuntime,
    gateway: RemoteGatewayClient,
    remote_session_id: String,
    state: RwLock<BridgeStateSnapshot>,
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
                devbox_runtime_manager: config.devbox.clone().map(DevboxRuntimeManager::new),
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
            .create_session_backend(&id, model, resume_thread_id, &metadata)
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

    pub async fn list_threads(&self, params: Value) -> Result<Value, AppError> {
        if let Some(session) = self.first_session() {
            session.metadata.touch(self.inner.config.session_ttl);
            info!(session_id = %session.id, "listing threads with active session backend");
            return session.list_threads(params).await;
        }

        info!("listing threads with transient bridge");
        let bridge = self.new_transient_bridge();
        bridge.start_without_thread().await?;
        let result = bridge.list_threads(params).await;
        let stop_result = bridge.stop().await;
        if let Err(error) = stop_result {
            error!("failed to stop transient app-server bridge: {error}");
        }
        result
    }

    pub async fn read_thread(&self, thread_id: &str) -> Result<Value, AppError> {
        if let Some(session) = self.first_session() {
            session.metadata.touch(self.inner.config.session_ttl);
            info!(
                session_id = %session.id,
                thread_id = %thread_id,
                "reading thread with active session backend"
            );
            return session.read_thread(thread_id).await;
        }

        info!(thread_id = %thread_id, "reading thread with transient bridge");
        let bridge = self.new_transient_bridge();
        bridge.start_without_thread().await?;
        let result = bridge.read_thread(thread_id).await;
        let stop_result = bridge.stop().await;
        if let Err(error) = stop_result {
            error!("failed to stop transient app-server bridge: {error}");
        }
        result
    }

    pub async fn read_thread_with_session(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<Value, AppError> {
        let session = self.require_session(session_id)?;
        info!(
            session_id = %session.id,
            thread_id = %thread_id,
            "reading thread with deployment session backend"
        );
        session.read_thread(thread_id).await
    }

    pub fn get_state(&self, session_id: &str) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        Ok(session.state())
    }

    pub fn get_session_info(&self, session_id: &str) -> Result<SessionInfo, AppError> {
        let session = self.require_session(session_id)?;
        Ok(session.info())
    }

    pub fn touch_session_for(&self, session_id: &str, ttl: Duration) -> Result<(), AppError> {
        let session = self.require_session(session_id)?;
        session.metadata.touch(ttl);
        if session.is_devbox_runtime() {
            session.refresh_devbox_lease(ttl)?;
        }
        Ok(())
    }

    pub fn is_devbox_runtime_session(&self, session_id: &str) -> Result<bool, AppError> {
        let session = self.require_session(session_id)?;
        Ok(session.is_devbox_runtime())
    }

    pub fn subscribe(
        &self,
        session_id: &str,
    ) -> Result<(SessionInfo, BridgeStateSnapshot, Receiver<BridgeEvent>), AppError> {
        let session = self.require_session(session_id)?;
        info!(session_id = %session_id, "subscribing to session events");
        let receiver = match &session.backend {
            SessionBackend::Local { bridge } => bridge.subscribe(),
            SessionBackend::RemoteDevbox(_) => {
                return Err(AppError::internal(
                    "Remote Devbox session events are not proxied yet",
                ));
            }
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

    pub async fn start_new_thread(
        &self,
        session_id: &str,
        model: Option<String>,
    ) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        info!(
            session_id = %session_id,
            model = model.as_deref().unwrap_or("-"),
            "forwarding new thread request to bridge"
        );
        session.start_new_thread(model).await
    }

    pub async fn resume_thread(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<BridgeStateSnapshot, AppError> {
        let session = self.require_session(session_id)?;
        info!(
            session_id = %session_id,
            thread_id = %thread_id,
            "forwarding thread resume request to bridge"
        );
        session.resume_thread(thread_id).await
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

    async fn create_session_backend(
        &self,
        session_id: &str,
        model: Option<String>,
        resume_thread_id: Option<String>,
        metadata: &Arc<SessionMetadata>,
    ) -> Result<SessionBackend, AppError> {
        if self.inner.config.session_runtime == SessionRuntimeMode::Devbox {
            return self
                .create_remote_devbox_backend(session_id, model, resume_thread_id)
                .await;
        }

        self.create_local_backend(model, resume_thread_id, metadata)
            .await
    }

    async fn create_local_backend(
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

        Ok(SessionBackend::Local { bridge })
    }

    async fn create_remote_devbox_backend(
        &self,
        session_id: &str,
        model: Option<String>,
        resume_thread_id: Option<String>,
    ) -> Result<SessionBackend, AppError> {
        let manager = self
            .inner
            .devbox_runtime_manager
            .as_ref()
            .ok_or_else(|| AppError::internal("Devbox runtime manager is not configured"))?;

        info!(session_id = %session_id, "creating devbox runtime for session");
        let runtime = manager
            .create_for_session(session_id, self.inner.config.session_ttl)
            .await?;
        info!("devbox runtime ready for session");

        let gateway_url = runtime
            .gateway_url
            .clone()
            .ok_or_else(|| AppError::internal("Devbox gateway URL is not available"))?;
        let gateway_auth_token = runtime
            .gateway_auth_token
            .clone()
            .or_else(|| create_gateway_auth_token(self.inner.config.auth.as_ref()));
        let gateway = RemoteGatewayClient::with_auth_token(gateway_url, gateway_auth_token);
        if let Err(error) = gateway
            .wait_for_ready(manager.config().gateway_ready_timeout)
            .await
        {
            runtime.cleanup_after_create_failure().await;
            return Err(error.into());
        }
        let RemoteSession { id, info: _, state } =
            match gateway.create_session(model, resume_thread_id).await {
                Ok(session) => session,
                Err(error) => {
                    runtime.cleanup_after_create_failure().await;
                    return Err(error.into());
                }
            };

        let backend = RemoteDevboxBackend {
            runtime,
            gateway,
            remote_session_id: id,
            state: RwLock::new(state),
        };

        Ok(SessionBackend::RemoteDevbox(Box::new(backend)))
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

    fn first_session(&self) -> Option<Arc<Session>> {
        self.inner.sessions.read().unwrap().values().next().cloned()
    }

    fn new_transient_bridge(&self) -> CodexAppServerBridge {
        CodexAppServerBridge::new(BridgeOptions {
            cwd: self.inner.config.bridge_cwd.clone(),
            codex_bin: self.inner.config.codex_bin.clone(),
            debug: self.inner.config.debug,
            client_info: self.inner.config.client_info.clone(),
            default_model: self.inner.config.default_model.clone(),
            activity_touch: Arc::new(|| {}),
        })
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

#[derive(Debug, Serialize)]
struct GatewayAuthClaims {
    exp: usize,
    iat: usize,
}

fn create_gateway_auth_token(auth: Option<&AuthConfig>) -> Option<String> {
    let auth = auth?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as usize)
        .unwrap_or_default();
    let claims = GatewayAuthClaims {
        iat: now,
        exp: now + 24 * 60 * 60,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(auth.jwt_secret.as_bytes()),
    )
    .ok()
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
            SessionBackend::Local { bridge } => bridge.get_state(),
            SessionBackend::RemoteDevbox(backend) => backend.state.read().unwrap().clone(),
        }
    }

    fn is_devbox_runtime(&self) -> bool {
        matches!(self.backend, SessionBackend::RemoteDevbox(_))
    }

    fn refresh_devbox_lease(&self, ttl: Duration) -> Result<(), AppError> {
        match &self.backend {
            SessionBackend::Local { .. } => Ok(()),
            SessionBackend::RemoteDevbox(backend) => {
                let runtime = backend.runtime.clone();
                tokio::spawn(async move {
                    if runtime.refresh_pause_for(ttl).await.is_err() {
                        warn!("devbox runtime lease refresh failed");
                    }
                });
                Ok(())
            }
        }
    }

    async fn list_threads(&self, params: Value) -> Result<Value, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => bridge.list_threads(params).await,
            SessionBackend::RemoteDevbox(backend) => {
                Ok(backend.gateway.list_threads(params).await?)
            }
        }
    }

    async fn read_thread(&self, thread_id: &str) -> Result<Value, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => bridge.read_thread(thread_id).await,
            SessionBackend::RemoteDevbox(backend) => {
                Ok(backend.gateway.read_thread(thread_id).await?)
            }
        }
    }

    async fn send_prompt(&self, prompt: &str) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => {
                bridge.send_prompt(prompt).await?;
                Ok(bridge.get_state())
            }
            SessionBackend::RemoteDevbox(backend) => {
                let snapshot = backend
                    .gateway
                    .send_prompt(&backend.remote_session_id, prompt)
                    .await?;
                *backend.state.write().unwrap() = snapshot.clone();
                Ok(snapshot)
            }
        }
    }

    async fn interrupt_turn(&self) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => {
                bridge.interrupt_turn().await?;
                Ok(bridge.get_state())
            }
            SessionBackend::RemoteDevbox(backend) => {
                let snapshot = backend
                    .gateway
                    .interrupt_turn(&backend.remote_session_id)
                    .await?;
                *backend.state.write().unwrap() = snapshot.clone();
                Ok(snapshot)
            }
        }
    }

    async fn start_new_thread(
        &self,
        model: Option<String>,
    ) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => {
                bridge.start_new_thread(model).await?;
                Ok(bridge.get_state())
            }
            SessionBackend::RemoteDevbox(backend) => {
                let snapshot = backend
                    .gateway
                    .start_new_thread(&backend.remote_session_id, model)
                    .await?;
                *backend.state.write().unwrap() = snapshot.clone();
                Ok(snapshot)
            }
        }
    }

    async fn resume_thread(&self, thread_id: &str) -> Result<BridgeStateSnapshot, AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => {
                bridge.resume_thread(thread_id).await?;
                Ok(bridge.get_state())
            }
            SessionBackend::RemoteDevbox(backend) => {
                let snapshot = backend
                    .gateway
                    .resume_thread(&backend.remote_session_id, thread_id)
                    .await?;
                *backend.state.write().unwrap() = snapshot.clone();
                Ok(snapshot)
            }
        }
    }

    async fn close(&self, reason: &str) -> Result<(), AppError> {
        match &self.backend {
            SessionBackend::Local { bridge } => {
                bridge.broadcast_session_closed(&self.id, reason);
                bridge.stop().await
            }
            SessionBackend::RemoteDevbox(backend) => {
                if backend
                    .gateway
                    .delete_session(&backend.remote_session_id)
                    .await
                    .is_err()
                {
                    warn!("remote devbox session cleanup failed");
                }
                if backend.runtime.delete().await.is_err() {
                    warn!("devbox runtime cleanup failed");
                }
                Ok(())
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
