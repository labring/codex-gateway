use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, info};

use crate::models::{BridgeStateSnapshot, SessionInfo};

const REMOTE_GATEWAY_TIMEOUT: Duration = Duration::from_secs(60);
const REMOTE_GATEWAY_READY_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum RemoteGatewayError {
    #[error("Remote gateway URL is not available from Devbox runtime")]
    MissingUrl,
    #[error("Remote gateway did not become ready before timeout")]
    ReadyTimeout,
    #[error("Remote gateway request failed with status {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("Remote gateway returned an invalid response")]
    InvalidResponse,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone)]
pub struct RemoteGatewayClient {
    base_url: String,
    auth_token: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct RemoteSession {
    pub id: String,
    pub info: SessionInfo,
    pub state: BridgeStateSnapshot,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    session_id: String,
    session: SessionInfo,
    state: BridgeStateSnapshot,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    session: SessionInfo,
    state: BridgeStateSnapshot,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: Option<String>,
}

impl RemoteGatewayClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth_token: None,
            http: reqwest::Client::new(),
        }
    }

    pub fn with_auth_token(base_url: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth_token,
            http: reqwest::Client::new(),
        }
    }

    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<(), RemoteGatewayError> {
        let started = Instant::now();

        while started.elapsed() < timeout {
            let health = self
                .request_json::<Value>("/healthz", reqwest::Method::GET, Value::Null)
                .await;
            if health
                .ok()
                .and_then(|payload| payload.get("ok").and_then(Value::as_bool))
                == Some(true)
            {
                let ready = self
                    .request_json::<Value>("/readyz", reqwest::Method::GET, Value::Null)
                    .await;
                if ready
                    .ok()
                    .and_then(|payload| payload.get("ok").and_then(Value::as_bool))
                    == Some(true)
                {
                    return Ok(());
                }
            }

            sleep(REMOTE_GATEWAY_READY_POLL).await;
        }

        Err(RemoteGatewayError::ReadyTimeout)
    }

    pub async fn create_session(
        &self,
        model: Option<String>,
        resume_thread_id: Option<String>,
    ) -> Result<RemoteSession, RemoteGatewayError> {
        info!("remote gateway session create request started");
        let response = self
            .request_json::<SessionResponse>(
                "/api/sessions",
                reqwest::Method::POST,
                json!({
                    "model": model,
                    "resumeThreadId": resume_thread_id,
                }),
            )
            .await;
        if response.is_err() {
            error!("remote gateway session create request failed");
        } else {
            info!("remote gateway session create request finished");
        }

        response.map(|response| RemoteSession {
            id: response.session_id,
            info: response.session,
            state: response.state,
        })
    }

    pub async fn get_state(&self, session_id: &str) -> Result<RemoteSession, RemoteGatewayError> {
        let response = self
            .request_json::<StateResponse>(
                &format!("/api/sessions/{}/state", encode_path_segment(session_id)),
                reqwest::Method::GET,
                Value::Null,
            )
            .await?;

        Ok(RemoteSession {
            id: session_id.to_string(),
            info: response.session,
            state: response.state,
        })
    }

    pub async fn send_prompt(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<BridgeStateSnapshot, RemoteGatewayError> {
        let response = self
            .request_json::<StateResponse>(
                &format!("/api/sessions/{}/turn", encode_path_segment(session_id)),
                reqwest::Method::POST,
                json!({ "prompt": prompt }),
            )
            .await?;

        Ok(response.state)
    }

    pub async fn interrupt_turn(
        &self,
        session_id: &str,
    ) -> Result<BridgeStateSnapshot, RemoteGatewayError> {
        let response = self
            .request_json::<StateResponse>(
                &format!(
                    "/api/sessions/{}/turn/interrupt",
                    encode_path_segment(session_id)
                ),
                reqwest::Method::POST,
                Value::Null,
            )
            .await?;

        Ok(response.state)
    }

    pub async fn start_new_thread(
        &self,
        session_id: &str,
        model: Option<String>,
    ) -> Result<BridgeStateSnapshot, RemoteGatewayError> {
        let response = self
            .request_json::<StateResponse>(
                &format!(
                    "/api/sessions/{}/thread/new",
                    encode_path_segment(session_id)
                ),
                reqwest::Method::POST,
                json!({ "model": model }),
            )
            .await?;

        Ok(response.state)
    }

    pub async fn resume_thread(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<BridgeStateSnapshot, RemoteGatewayError> {
        let response = self
            .request_json::<StateResponse>(
                &format!(
                    "/api/sessions/{}/thread/resume",
                    encode_path_segment(session_id)
                ),
                reqwest::Method::POST,
                json!({ "threadId": thread_id }),
            )
            .await?;

        Ok(response.state)
    }

    pub async fn read_thread(&self, thread_id: &str) -> Result<Value, RemoteGatewayError> {
        let response = self
            .request_json::<Value>(
                &format!("/api/threads/{}", encode_path_segment(thread_id)),
                reqwest::Method::GET,
                Value::Null,
            )
            .await?;

        Ok(unwrap_raw_response(response))
    }

    pub async fn list_threads(&self, params: Value) -> Result<Value, RemoteGatewayError> {
        let path = match query_string(params) {
            Some(query) => format!("/api/threads?{query}"),
            None => "/api/threads".to_string(),
        };

        let response = self
            .request_json::<Value>(&path, reqwest::Method::GET, Value::Null)
            .await?;

        Ok(unwrap_raw_response(response))
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), RemoteGatewayError> {
        let _: Value = self
            .request_json(
                &format!("/api/sessions/{}", encode_path_segment(session_id)),
                reqwest::Method::DELETE,
                Value::Null,
            )
            .await?;
        Ok(())
    }

    async fn request_json<T>(
        &self,
        path: &str,
        method: reqwest::Method,
        body: Value,
    ) -> Result<T, RemoteGatewayError>
    where
        T: for<'de> Deserialize<'de>,
    {
        if self.base_url.is_empty() {
            return Err(RemoteGatewayError::MissingUrl);
        }

        let url = format!("{}{}", self.base_url, path);
        let mut request = self
            .http
            .request(method.clone(), url)
            .timeout(REMOTE_GATEWAY_TIMEOUT);
        if let Some(token) = self.auth_token.as_deref().filter(|token| !token.is_empty()) {
            request = request.bearer_auth(token);
        }
        if method != reqwest::Method::GET && body != Value::Null {
            request = request.json(&body);
        }

        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            let message = serde_json::from_slice::<ErrorResponse>(&bytes)
                .ok()
                .and_then(|payload| payload.error)
                .unwrap_or_else(|| "Remote gateway request failed".to_string());
            return Err(RemoteGatewayError::Api { status, message });
        }

        serde_json::from_slice::<T>(&bytes).map_err(|_| RemoteGatewayError::InvalidResponse)
    }
}

fn query_string(params: Value) -> Option<String> {
    let object = params.as_object()?;
    let parts = object
        .iter()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::String(value) if !value.is_empty() => value.clone(),
                Value::Number(value) => value.to_string(),
                Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            Some(format!(
                "{}={}",
                encode_query_component(key),
                encode_query_component(&value)
            ))
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("&"))
    }
}

fn unwrap_raw_response(response: Value) -> Value {
    response.get("raw").cloned().unwrap_or(response)
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

fn encode_query_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            b' ' => vec!['+'],
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn query_string_keeps_supported_thread_list_filters() {
        let query = query_string(json!({
            "limit": 20,
            "archived": false,
            "searchTerm": "hello world",
            "ignored": null
        }))
        .expect("query string");

        assert!(query.contains("limit=20"));
        assert!(query.contains("archived=false"));
        assert!(query.contains("searchTerm=hello+world"));
        assert!(!query.contains("ignored"));
    }

    #[test]
    fn unwraps_public_thread_read_response_to_raw_app_server_shape() {
        let response = json!({
            "ok": true,
            "threadId": "thread-1",
            "thread": {
                "id": "thread-1"
            },
            "raw": {
                "thread": {
                    "id": "thread-1",
                    "turns": []
                }
            }
        });

        let raw = unwrap_raw_response(response);

        assert_eq!(
            raw.pointer("/thread/id").and_then(Value::as_str),
            Some("thread-1")
        );
    }
}
