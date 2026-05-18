use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub platform_family: Option<String>,
    pub platform_os: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSnapshot {
    pub raw: Option<Value>,
    pub summary: String,
    pub requires_openai_auth: Option<bool>,
}

impl Default for AccountSnapshot {
    fn default() -> Self {
        Self {
            raw: None,
            summary: "none".to_string(),
            requires_openai_auth: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub model: String,
    pub display_name: Option<String>,
    pub is_default: bool,
    pub hidden: bool,
    pub supports_personality: bool,
    pub input_modalities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    pub id: String,
    pub role: String,
    pub text: String,
    pub status: String,
    pub source: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEvent {
    pub at: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub method: Option<String>,
    pub item_type: Option<String>,
    pub item_id: Option<String>,
    pub status: Option<String>,
    pub text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeStateSnapshot {
    pub ready: bool,
    pub cwd: String,
    pub started_at: Option<String>,
    pub runtime: RuntimeInfo,
    pub account: AccountSnapshot,
    pub models: Vec<ModelInfo>,
    pub selected_model: Option<String>,
    pub thread_id: Option<String>,
    pub thread_status: Option<Value>,
    pub current_turn_id: Option<String>,
    pub active_turn: bool,
    pub last_turn_status: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub recent_events: Vec<SummaryEvent>,
}

impl BridgeStateSnapshot {
    pub fn new(cwd: String) -> Self {
        Self {
            ready: false,
            cwd,
            started_at: None,
            runtime: RuntimeInfo::default(),
            account: AccountSnapshot::default(),
            models: Vec::new(),
            selected_model: None,
            thread_id: None,
            thread_status: None,
            current_turn_id: None,
            active_turn: false,
            last_turn_status: None,
            transcript: Vec::new(),
            recent_events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub last_access_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosedEvent {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcErrorPayload {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestEvent {
    pub method: String,
    pub params: Value,
    pub handled: bool,
    pub result: Option<String>,
    pub error: Option<RpcErrorPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarningEvent {
    #[serde(rename = "type")]
    pub warning_type: String,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BridgeEvent {
    State(Box<BridgeStateSnapshot>),
    Notification(Value),
    ServerRequest(ServerRequestEvent),
    Warning(WarningEvent),
    Raw(String),
    SessionClosed(SessionClosedEvent),
}
