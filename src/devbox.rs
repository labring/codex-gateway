use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, info};

use crate::config::DevboxConfig;
use crate::env_config::{
    DEFAULT_MODEL_ENV, JWT_SECRET_ENV, OPENAI_API_KEY_ENV, OPENAI_BASE_URL_ENV, read_env,
};

pub const SEAKILLS_INSTALL_COMMAND: &str =
    "npx --yes skills add https://github.com/zjy365/seakills/tree/sandbox-skill-lite -y";

const DEVBOX_API_PREFIX: &str = "/api/v1/devbox";
const DEVBOX_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEVBOX_EXEC_REQUEST_BUFFER: Duration = Duration::from_secs(10);
const DEVBOX_WAIT_POLL: Duration = Duration::from_secs(2);
const DEVBOX_SECRET_READY_RETRIES: usize = 3;
const DEVBOX_SECRET_READY_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum DevboxError {
    #[error("Devbox runtime is not configured")]
    NotConfigured,
    #[error("Devbox base URL is not configured")]
    MissingBaseUrl,
    #[error("Devbox auth is not configured")]
    MissingAuth,
    #[error("Devbox API request failed with status {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("Devbox API returned an invalid response")]
    InvalidResponse,
    #[error("Devbox runtime did not become running before timeout")]
    WaitTimeout,
    #[error("Devbox bootstrap command failed")]
    BootstrapFailed,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Jwt(#[from] jsonwebtoken::errors::Error),
}

#[derive(Debug, Clone)]
pub struct DevboxClient {
    config: Arc<DevboxConfig>,
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct DevboxRuntime {
    pub name: String,
    pub namespace: String,
    pub gateway_url: Option<String>,
    pub gateway_auth_token: Option<String>,
    bootstrap_timeout: Duration,
    client: DevboxClient,
}

#[derive(Debug, Clone)]
pub struct DevboxRuntimeManager {
    client: DevboxClient,
    config: DevboxConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JwtClaims {
    namespace: String,
    iat: usize,
    exp: usize,
}

#[derive(Debug, Deserialize)]
pub struct DevboxEnvelope<T> {
    pub data: T,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevboxState {
    pub phase: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevboxGatewayInfo {
    pub url: Option<String>,
    pub route: Option<String>,
    pub external_url: Option<String>,
    pub app_url: Option<String>,
    pub access_url: Option<String>,
    pub token: Option<String>,
    pub jwt: Option<String>,
    pub auth_token: Option<String>,
    pub bearer_token: Option<String>,
    pub access_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevboxInfo {
    pub name: String,
    pub creation_timestamp: Option<String>,
    pub deletion_timestamp: Option<String>,
    pub state: DevboxState,
    pub gateway: Option<DevboxGatewayInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDevboxKubeAccess {
    pub enabled: bool,
    pub role_template: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateDevboxLabel {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDevboxInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(rename = "upstreamID")]
    pub upstream_id: String,
    pub kube_access: CreateDevboxKubeAccess,
    pub env: serde_json::Value,
    pub pause_at: String,
    pub archive_after_pause_time: String,
    pub labels: Vec<CreateDevboxLabel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDevboxResult {
    pub name: String,
    pub namespace: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevboxExecInput {
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevboxExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub executed_at: String,
}

impl DevboxClient {
    pub fn new(config: DevboxConfig) -> Self {
        Self {
            config: Arc::new(config),
            http: reqwest::Client::new(),
        }
    }

    pub async fn create(
        &self,
        input: &CreateDevboxInput,
    ) -> Result<CreateDevboxResult, DevboxError> {
        info!("devbox create request started");
        let result = self
            .request(
                "",
                reqwest::Method::POST,
                Some(input),
                DEVBOX_REQUEST_TIMEOUT,
            )
            .await;
        if result.is_err() {
            error!("devbox create request failed");
        } else {
            info!("devbox create request finished");
        }
        result
    }

    pub async fn get(&self, name: &str) -> Result<DevboxInfo, DevboxError> {
        self.get_with_secret_retry(name).await
    }

    pub async fn exec(
        &self,
        name: &str,
        input: &DevboxExecInput,
    ) -> Result<DevboxExecOutput, DevboxError> {
        info!("devbox exec request started");
        let timeout =
            Duration::from_secs(input.timeout_seconds.unwrap_or(60)) + DEVBOX_EXEC_REQUEST_BUFFER;
        let result = self
            .request(
                &format!("/{}/exec", encode_path_segment(name)),
                reqwest::Method::POST,
                Some(input),
                timeout.max(DEVBOX_REQUEST_TIMEOUT),
            )
            .await;
        if result.is_err() {
            error!("devbox exec request failed");
        } else {
            info!("devbox exec request finished");
        }
        result
    }

    pub async fn delete(&self, name: &str) -> Result<(), DevboxError> {
        let _: serde_json::Value = self
            .request(
                &format!("/{}", encode_path_segment(name)),
                reqwest::Method::DELETE,
                Option::<&serde_json::Value>::None,
                DEVBOX_REQUEST_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn refresh_pause(&self, name: &str, pause_at: String) -> Result<(), DevboxError> {
        let _: serde_json::Value = self
            .request(
                &format!("/{}/pause/refresh", encode_path_segment(name)),
                reqwest::Method::POST,
                Some(&json!({ "pauseAt": pause_at })),
                DEVBOX_REQUEST_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn wait_for_running(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<DevboxInfo, DevboxError> {
        let started = std::time::Instant::now();

        while started.elapsed() < timeout {
            let info = self.get(name).await?;
            if info.state.phase == "Running" {
                return Ok(info);
            }
            sleep(DEVBOX_WAIT_POLL).await;
        }

        Err(DevboxError::WaitTimeout)
    }

    async fn get_with_secret_retry(&self, name: &str) -> Result<DevboxInfo, DevboxError> {
        let mut attempt = 0;

        loop {
            let result = self
                .request::<(), DevboxInfo>(
                    &format!("/{}", encode_path_segment(name)),
                    reqwest::Method::GET,
                    None,
                    DEVBOX_REQUEST_TIMEOUT,
                )
                .await;

            match result {
                Err(error)
                    if is_secret_pending_error(&error) && attempt < DEVBOX_SECRET_READY_RETRIES =>
                {
                    attempt += 1;
                    sleep(DEVBOX_SECRET_READY_RETRY_DELAY).await;
                }
                other => return other,
            }
        }
    }

    async fn request<B, T>(
        &self,
        path: &str,
        method: reqwest::Method,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, DevboxError>
    where
        B: Serialize + ?Sized,
        T: for<'de> Deserialize<'de>,
    {
        let token = self.auth_token()?;
        let url = self.url(path)?;
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .timeout(timeout);

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        let status = response.status();
        let payload = response
            .json::<DevboxEnvelope<T>>()
            .await
            .map_err(|_| DevboxError::InvalidResponse)?;

        if !status.is_success() {
            return Err(DevboxError::Api {
                status,
                message: payload
                    .message
                    .unwrap_or_else(|| "Devbox API request failed".to_string()),
            });
        }

        Ok(payload.data)
    }

    fn url(&self, path: &str) -> Result<String, DevboxError> {
        if self.config.base_url.trim().is_empty() {
            return Err(DevboxError::MissingBaseUrl);
        }

        let base = self.config.base_url.trim().trim_end_matches('/');
        Ok(format!("{base}{DEVBOX_API_PREFIX}{path}"))
    }

    fn auth_token(&self) -> Result<String, DevboxError> {
        if let Some(token) = self
            .config
            .token
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            return Ok(token.to_string());
        }

        let Some(signing_key) = self
            .config
            .jwt_signing_key
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            return Err(DevboxError::MissingAuth);
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as usize)
            .unwrap_or_default();
        let claims = JwtClaims {
            namespace: self.config.namespace.clone(),
            iat: now,
            exp: now + self.config.jwt_ttl_seconds as usize,
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(signing_key.as_bytes()),
        )
        .map_err(DevboxError::from)
    }
}

impl DevboxRuntimeManager {
    pub fn new(config: DevboxConfig) -> Self {
        let client = DevboxClient::new(config.clone());
        Self { client, config }
    }

    pub fn config(&self) -> &DevboxConfig {
        &self.config
    }

    pub async fn create_for_session(
        &self,
        session_id: &str,
        ttl: Duration,
    ) -> Result<DevboxRuntime, DevboxError> {
        let name = create_runtime_name(session_id);
        let upstream_id = create_upstream_id(session_id);
        let pause_at = Utc::now()
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::minutes(30));

        let create = CreateDevboxInput {
            name: name.clone(),
            image: self.config.runtime_image.clone(),
            upstream_id,
            kube_access: CreateDevboxKubeAccess {
                enabled: true,
                role_template: "edit".to_string(),
            },
            env: create_runtime_env(ttl),
            pause_at: pause_at.to_rfc3339(),
            archive_after_pause_time: self.config.archive_after_pause_time.clone(),
            labels: vec![
                CreateDevboxLabel {
                    key: "app.kubernetes.io/component".to_string(),
                    value: "runtime".to_string(),
                },
                CreateDevboxLabel {
                    key: "app.kubernetes.io/managed-by".to_string(),
                    value: "codex-gateway".to_string(),
                },
            ],
        };

        let response = self.client.create(&create).await?;
        let info = match self
            .client
            .wait_for_running(&name, self.config.wait_timeout)
            .await
        {
            Ok(info) => info,
            Err(error) => {
                self.cleanup_failed_runtime(&name).await;
                return Err(error);
            }
        };
        let runtime = DevboxRuntime {
            name,
            namespace: response.namespace,
            gateway_url: resolve_gateway_url(&info),
            gateway_auth_token: resolve_gateway_auth_token(&info),
            bootstrap_timeout: self.config.bootstrap_timeout,
            client: self.client.clone(),
        };

        if let Err(error) = runtime.bootstrap().await {
            runtime.cleanup_after_create_failure().await;
            return Err(error);
        }
        Ok(runtime)
    }

    async fn cleanup_failed_runtime(&self, name: &str) {
        if self.client.delete(name).await.is_err() {
            error!("failed to cleanup devbox runtime after create failure");
        }
    }
}

impl DevboxRuntime {
    pub async fn exec(&self, request: DevboxExecInput) -> Result<DevboxExecOutput, DevboxError> {
        self.client.exec(&self.name, &request).await
    }

    pub async fn exec_shell(
        &self,
        script: impl Into<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<DevboxExecOutput, DevboxError> {
        self.exec(DevboxExecInput {
            command: vec!["sh".to_string(), "-lc".to_string(), script.into()],
            stdin: None,
            timeout_seconds,
            container: None,
        })
        .await
    }

    async fn bootstrap(&self) -> Result<(), DevboxError> {
        info!("devbox bootstrap command started");
        let output = self
            .exec_shell(
                SEAKILLS_INSTALL_COMMAND,
                Some(self.bootstrap_timeout.as_secs()),
            )
            .await?;

        if output.exit_code != 0 {
            error!("devbox bootstrap command failed");
            return Err(DevboxError::BootstrapFailed);
        }

        info!("devbox bootstrap command finished");
        Ok(())
    }

    pub async fn delete(&self) -> Result<(), DevboxError> {
        self.client.delete(&self.name).await
    }

    pub async fn cleanup_after_create_failure(&self) {
        if self.delete().await.is_err() {
            error!("failed to cleanup devbox runtime after create failure");
        }
    }

    pub async fn refresh_pause_for(&self, ttl: Duration) -> Result<(), DevboxError> {
        let pause_at = Utc::now()
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::minutes(30));
        self.client
            .refresh_pause(&self.name, pause_at.to_rfc3339())
            .await
    }
}

fn resolve_gateway_url(info: &DevboxInfo) -> Option<String> {
    let gateway = info.gateway.as_ref()?;
    gateway
        .url
        .clone()
        .or_else(|| gateway.route.clone())
        .or_else(|| gateway.external_url.clone())
        .or_else(|| gateway.app_url.clone())
        .or_else(|| gateway.access_url.clone())
}

fn resolve_gateway_auth_token(info: &DevboxInfo) -> Option<String> {
    let gateway = info.gateway.as_ref()?;
    gateway
        .access_token
        .clone()
        .or_else(|| gateway.auth_token.clone())
        .or_else(|| gateway.bearer_token.clone())
        .or_else(|| gateway.token.clone())
        .or_else(|| gateway.jwt.clone())
}

fn create_runtime_env(ttl: Duration) -> serde_json::Value {
    let mut env = serde_json::Map::new();
    env.insert("CODEX_GATEWAY_HOST".to_string(), json!("0.0.0.0"));
    env.insert("CODEX_GATEWAY_PORT".to_string(), json!("1317"));
    env.insert(
        "CODEX_GATEWAY_SESSION_TTL_MS".to_string(),
        json!(ttl.as_millis().to_string()),
    );

    env.insert("CODEX_GATEWAY_CODEX_HOME".to_string(), json!("/codex-home"));

    for (target, source) in [
        ("CODEX_GATEWAY_MODEL", DEFAULT_MODEL_ENV),
        ("CODEX_GATEWAY_OPENAI_API_KEY", OPENAI_API_KEY_ENV),
        ("CODEX_GATEWAY_OPENAI_BASE_URL", OPENAI_BASE_URL_ENV),
        ("CODEX_GATEWAY_JWT_SECRET", JWT_SECRET_ENV),
    ] {
        if let Some(value) = read_env(source) {
            env.insert(target.to_string(), json!(value));
        }
    }

    serde_json::Value::Object(env)
}

fn is_secret_pending_error(error: &DevboxError) -> bool {
    match error {
        DevboxError::Api { status, message } => {
            status.is_server_error()
                && message.contains("get devbox private key failed")
                && message.contains("not found")
        }
        _ => false,
    }
}

fn create_runtime_name(session_id: &str) -> String {
    let digest = fnv1a(session_id.as_bytes());
    let alphabet = b"abcdefghijklmnopqrstuvwxyz";
    let mut value = digest;
    let mut name = String::with_capacity(12);

    for _ in 0..12 {
        name.push(alphabet[(value % alphabet.len() as u64) as usize] as char);
        value = value.rotate_right(5).wrapping_mul(0x100000001b3);
    }

    name
}

fn create_upstream_id(session_id: &str) -> String {
    if is_safe_upstream_id(session_id) {
        return session_id.to_string();
    }

    format!("session-{:016x}", fnv1a(session_id.as_bytes()))
}

fn is_safe_upstream_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    is_alnum(first)
        && is_alnum(last)
        && bytes
            .iter()
            .all(|byte| is_alnum(*byte) || matches!(*byte, b'-' | b'_' | b'.'))
}

fn is_alnum(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}
