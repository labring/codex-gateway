use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

const DEPLOYMENT_RESULT_PREFIX: &str = "DEPLOYMENT_RESULT:";
const BRAIN_DEPLOYMENT_SKILL_TRIGGER: &str = "/fulling-deploy";
const DEFAULT_BRAIN_MAX_ACTIVE_DEPLOYMENTS: usize = 4;
const DEFAULT_BRAIN_DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct BrainDeploymentRegistry {
    inner: Arc<Mutex<BrainDeploymentRegistryInner>>,
    max_active: usize,
    timeout: Duration,
}

struct BrainDeploymentRegistryInner {
    creating: usize,
    records: HashMap<String, BrainDeploymentRecord>,
}

#[derive(Debug, Clone)]
pub struct BrainDeploymentRecord {
    pub thread_id: String,
    pub session_id: String,
    pub repository: String,
    pub branch: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub terminal_status: Option<BrainDeploymentStatusResponse>,
}

pub struct BrainDeploymentCreateGuard {
    registry: BrainDeploymentRegistry,
    active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrainDeploymentStatusResponse {
    pub thread_id: String,
    pub status: String,
    pub message: String,
    pub image: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BrainDeploymentResultLine {
    status: String,
    image: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

enum BrainDeploymentResultState {
    Found(BrainDeploymentResultLine),
    Invalid(String),
    Missing,
}

impl BrainDeploymentRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BrainDeploymentRegistryInner {
                creating: 0,
                records: HashMap::new(),
            })),
            max_active: DEFAULT_BRAIN_MAX_ACTIVE_DEPLOYMENTS,
            timeout: DEFAULT_BRAIN_DEPLOYMENT_TIMEOUT,
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn try_begin_create(&self) -> Result<BrainDeploymentCreateGuard, AppError> {
        let mut inner = self.inner.lock().unwrap();
        let active = inner.creating + active_brain_deployments(&inner.records);
        if active >= self.max_active {
            return Err(AppError::service_unavailable(format!(
                "Maximum concurrent deployments reached ({})",
                self.max_active
            )));
        }

        inner.creating += 1;
        Ok(BrainDeploymentCreateGuard {
            registry: self.clone(),
            active: true,
        })
    }

    fn finish_create(&self, record: BrainDeploymentRecord) {
        let mut inner = self.inner.lock().unwrap();
        inner.creating = inner.creating.saturating_sub(1);
        inner.records.insert(record.thread_id.clone(), record);
    }

    fn cancel_create(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.creating = inner.creating.saturating_sub(1);
    }

    pub fn get(&self, thread_id: &str) -> Option<BrainDeploymentRecord> {
        self.inner.lock().unwrap().records.get(thread_id).cloned()
    }

    pub fn mark_terminal(
        &self,
        thread_id: &str,
        response: BrainDeploymentStatusResponse,
    ) -> Option<BrainDeploymentRecord> {
        let mut inner = self.inner.lock().unwrap();
        let record = inner.records.get_mut(thread_id)?;
        record.terminal_status = Some(response);
        Some(record.clone())
    }
}

impl BrainDeploymentRecord {
    fn new(
        thread_id: String,
        session_id: String,
        repository: String,
        branch: Option<String>,
        timeout: Duration,
    ) -> Self {
        let created_at = Utc::now();
        let expires_at = created_at + chrono::Duration::from_std(timeout).unwrap_or_default();
        Self {
            thread_id,
            session_id,
            repository,
            branch,
            created_at,
            expires_at,
            terminal_status: None,
        }
    }

    pub fn is_timed_out(&self) -> bool {
        self.terminal_status.is_none() && Utc::now() >= self.expires_at
    }

    pub fn timeout_response(&self) -> BrainDeploymentStatusResponse {
        BrainDeploymentStatusResponse {
            thread_id: self.thread_id.clone(),
            status: "failed".to_string(),
            message: "Deployment failed".to_string(),
            image: None,
            error: Some("Deployment timed out before producing a result".to_string()),
        }
    }

    pub fn stopped_response(&self) -> BrainDeploymentStatusResponse {
        BrainDeploymentStatusResponse {
            thread_id: self.thread_id.clone(),
            status: "failed".to_string(),
            message: "Deployment failed".to_string(),
            image: None,
            error: Some("Deployment session is no longer running".to_string()),
        }
    }
}

impl BrainDeploymentCreateGuard {
    pub fn complete(
        mut self,
        thread_id: String,
        session_id: String,
        repository: String,
        branch: Option<String>,
    ) -> BrainDeploymentRecord {
        self.active = false;
        let record = BrainDeploymentRecord::new(
            thread_id,
            session_id,
            repository,
            branch,
            self.registry.timeout,
        );
        self.registry.finish_create(record.clone());
        record
    }
}

impl Drop for BrainDeploymentCreateGuard {
    fn drop(&mut self) {
        if self.active {
            self.registry.cancel_create();
        }
    }
}

fn active_brain_deployments(records: &HashMap<String, BrainDeploymentRecord>) -> usize {
    let now = Utc::now();
    records
        .values()
        .filter(|record| record.terminal_status.is_none() && now < record.expires_at)
        .count()
}

pub fn build_brain_deployment_prompt(
    repository: &str,
    branch: Option<&str>,
    github_token: &str,
    skill_preinstalled: bool,
) -> String {
    let branch_instruction = branch
        .map(|branch| format!("Use branch `{branch}`."))
        .unwrap_or_else(|| "Use the repository default branch.".to_string());
    let skill_instruction = if skill_preinstalled {
        format!(
            "The deployment skill has already been installed by the gateway runtime bootstrap. Use the {BRAIN_DEPLOYMENT_SKILL_TRIGGER} deployment workflow if it is available. If it is unavailable, perform the equivalent workflow: inspect the repository, generate or reuse a Dockerfile, verify the image build, publish the image to GHCR, and report the pushed image reference."
        )
    } else {
        format!(
            r#"Mandatory first step:
- Install the deployment skill before doing anything else:
npx --yes skills add https://github.com/zjy365/seakills/tree/sandbox-skill-lite -y
- If the install command fails, stop and return a failed `DEPLOYMENT_RESULT` with the install failure reason.

After the skill is installed, use the {BRAIN_DEPLOYMENT_SKILL_TRIGGER} deployment workflow if it is available. If it is unavailable, perform the equivalent workflow: inspect the repository, generate or reuse a Dockerfile, verify the image build, publish the image to GHCR, and report the pushed image reference."#
        )
    };

    format!(
        r#"You are running a repository deployment requested through Codex Gateway.

{skill_instruction}

Repository: {repository}
{branch_instruction}
GitHub token: {github_token}

Constraints:
- Use the supplied GitHub token only for cloning, GitHub API access, and GHCR authentication.
- Do not print, quote, summarize, or otherwise repeat the GitHub token.
- Do not ask follow-up questions.
- Let the deployment workflow decide the GHCR image tag unless it already has a safer project-specific convention.
- Do not guess or fabricate the image reference. Only report an image that was actually pushed successfully.

Final machine-readable result:
- The final assistant message must contain exactly one result line.
- That line must start with `DEPLOYMENT_RESULT:` followed by compact JSON.
- The JSON object must contain `status`, `image`, `message`, and `error`.
- On success, use this exact one-line shape, replacing only the image and message values:
DEPLOYMENT_RESULT: {{"status":"succeeded","image":"ghcr.io/owner/repo:tag","message":"Deployment image pushed to GHCR","error":null}}
- On failure, use this exact one-line shape, replacing only the message and error values:
DEPLOYMENT_RESULT: {{"status":"failed","image":null,"message":"Deployment failed","error":"Concise failure reason"}}
- Do not wrap the result line in Markdown or add any other text after it."#
    )
}

pub fn brain_deployment_status_from_thread(
    thread_id: &str,
    thread_result: &Value,
) -> BrainDeploymentStatusResponse {
    let thread = thread_result.get("thread").unwrap_or(thread_result);

    match find_brain_deployment_result(thread) {
        BrainDeploymentResultState::Found(result) => {
            brain_deployment_response_from_result(thread_id, result)
        }
        BrainDeploymentResultState::Invalid(error) => BrainDeploymentStatusResponse {
            thread_id: thread_id.to_string(),
            status: "failed".to_string(),
            message: "Deployment failed".to_string(),
            image: None,
            error: Some(error),
        },
        BrainDeploymentResultState::Missing if thread_is_active(thread) => {
            BrainDeploymentStatusResponse {
                thread_id: thread_id.to_string(),
                status: "running".to_string(),
                message: "Deployment is still running".to_string(),
                image: None,
                error: None,
            }
        }
        BrainDeploymentResultState::Missing => BrainDeploymentStatusResponse {
            thread_id: thread_id.to_string(),
            status: "failed".to_string(),
            message: "Deployment failed".to_string(),
            image: None,
            error: Some("Deployment result was not found in thread history".to_string()),
        },
    }
}

fn brain_deployment_response_from_result(
    thread_id: &str,
    result: BrainDeploymentResultLine,
) -> BrainDeploymentStatusResponse {
    match result.status.trim().to_ascii_lowercase().as_str() {
        "succeeded" => {
            let image = trim_optional(result.image);
            if !image.as_deref().is_some_and(is_valid_ghcr_image) {
                return BrainDeploymentStatusResponse {
                    thread_id: thread_id.to_string(),
                    status: "failed".to_string(),
                    message: "Deployment failed".to_string(),
                    image: None,
                    error: Some("Deployment result did not include a valid GHCR image".to_string()),
                };
            }

            BrainDeploymentStatusResponse {
                thread_id: thread_id.to_string(),
                status: "succeeded".to_string(),
                message: trim_optional(result.message)
                    .unwrap_or_else(|| "Deployment image pushed to GHCR".to_string()),
                image,
                error: None,
            }
        }
        "failed" => {
            if result.image.is_some() {
                return BrainDeploymentStatusResponse {
                    thread_id: thread_id.to_string(),
                    status: "failed".to_string(),
                    message: "Deployment failed".to_string(),
                    image: None,
                    error: Some("Failed deployment result must not include an image".to_string()),
                };
            }

            BrainDeploymentStatusResponse {
                thread_id: thread_id.to_string(),
                status: "failed".to_string(),
                message: trim_optional(result.message)
                    .unwrap_or_else(|| "Deployment failed".to_string()),
                image: None,
                error: trim_optional(result.error)
                    .or_else(|| Some("Deployment failed without an error message".to_string())),
            }
        }
        other => BrainDeploymentStatusResponse {
            thread_id: thread_id.to_string(),
            status: "failed".to_string(),
            message: "Deployment failed".to_string(),
            image: None,
            error: Some(format!("Unsupported deployment result status: {other}")),
        },
    }
}

fn find_brain_deployment_result(thread: &Value) -> BrainDeploymentResultState {
    let Some(turns) = thread.get("turns").and_then(Value::as_array) else {
        return BrainDeploymentResultState::Missing;
    };

    for turn in turns.iter().rev() {
        if status_value_is_active(turn.get("status")) {
            continue;
        }

        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };

        for item in items.iter().rev() {
            if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
                continue;
            }

            for text in agent_message_texts(item).into_iter().rev() {
                match parse_brain_deployment_result_from_text(&text) {
                    Ok(Some(result)) => return BrainDeploymentResultState::Found(result),
                    Ok(None) => {}
                    Err(error) => return BrainDeploymentResultState::Invalid(error),
                }
            }
        }
    }

    BrainDeploymentResultState::Missing
}

fn agent_message_texts(item: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        texts.push(text.to_string());
    }

    if let Some(parts) = item.get("content").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                texts.push(text.to_string());
            }
        }
    }

    texts
}

fn parse_brain_deployment_result_from_text(
    text: &str,
) -> Result<Option<BrainDeploymentResultLine>, String> {
    let lines = text.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate().rev() {
        let Some(json_text) = line.strip_prefix(DEPLOYMENT_RESULT_PREFIX) else {
            continue;
        };
        if lines[index + 1..]
            .iter()
            .any(|tail_line| !tail_line.trim().is_empty())
        {
            return Err("Deployment result line must be the final non-empty line".to_string());
        }

        let json_text = json_text.trim();
        if json_text.is_empty() {
            return Err("Deployment result line did not include JSON".to_string());
        }
        let value = serde_json::from_str::<Value>(json_text)
            .map_err(|error| format!("Failed to parse deployment result JSON: {error}"))?;
        for key in ["status", "image", "message", "error"] {
            if value.get(key).is_none() {
                return Err(format!("Deployment result JSON is missing `{key}`"));
            }
        }

        return serde_json::from_value(value)
            .map(Some)
            .map_err(|error| format!("Invalid deployment result JSON shape: {error}"));
    }

    Ok(None)
}

fn is_valid_ghcr_image(image: &str) -> bool {
    let image = image.trim();
    let Some(rest) = image.strip_prefix("ghcr.io/") else {
        return false;
    };
    if rest.is_empty() || rest.ends_with(":tag") {
        return false;
    }
    if image.chars().any(char::is_whitespace) {
        return false;
    }

    rest.contains('/')
        && (rest
            .rsplit('/')
            .next()
            .is_some_and(|name| name.contains(':'))
            || rest.contains("@sha256:"))
}

fn thread_is_active(thread: &Value) -> bool {
    if status_value_is_active(thread.get("status")) {
        return true;
    }

    thread
        .get("turns")
        .and_then(Value::as_array)
        .is_some_and(|turns| {
            turns
                .iter()
                .any(|turn| status_value_is_active(turn.get("status")))
        })
}

fn status_value_is_active(status: Option<&Value>) -> bool {
    match status {
        Some(Value::String(value)) => status_is_active(value),
        Some(Value::Object(object)) => object
            .get("type")
            .or_else(|| object.get("status"))
            .and_then(Value::as_str)
            .is_some_and(status_is_active),
        _ => false,
    }
}

fn status_is_active(status: &str) -> bool {
    matches!(
        status
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-'], "")
            .as_str(),
        "active" | "inprogress" | "running" | "queued" | "pending" | "started"
    )
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deployment_prompt_includes_exact_result_shapes() {
        let prompt = build_brain_deployment_prompt("owner/repo", Some("main"), "ghp_secret", false);

        assert!(prompt.contains("Repository: owner/repo"));
        assert!(prompt.contains("Use branch `main`."));
        assert!(prompt.contains("GitHub token: ghp_secret"));
        assert!(prompt.contains(
            "npx --yes skills add https://github.com/zjy365/seakills/tree/sandbox-skill-lite -y"
        ));
        assert!(prompt.contains("Mandatory first step"));
        assert!(prompt.contains("Do not guess or fabricate the image reference"));
        assert!(prompt.contains(
            r#"DEPLOYMENT_RESULT: {"status":"succeeded","image":"ghcr.io/owner/repo:tag","message":"Deployment image pushed to GHCR","error":null}"#
        ));
        assert!(prompt.contains(
            r#"DEPLOYMENT_RESULT: {"status":"failed","image":null,"message":"Deployment failed","error":"Concise failure reason"}"#
        ));
        assert!(prompt.contains("Do not wrap the result line in Markdown"));
    }

    #[test]
    fn deployment_prompt_skips_skill_install_when_runtime_bootstrapped() {
        let prompt = build_brain_deployment_prompt("owner/repo", None, "ghp_secret", true);

        assert!(prompt.contains("deployment skill has already been installed"));
        assert!(!prompt.contains("npx --yes skills add"));
        assert!(!prompt.contains("Mandatory first step"));
    }

    #[test]
    fn deployment_status_ignores_user_prompt_markers() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "userMessage",
                                "content": [
                                    {
                                        "type": "text",
                                        "text": "DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/wrong/image:tag\",\"message\":\"wrong\",\"error\":null}"
                                    }
                                ]
                            },
                            {
                                "type": "agentMessage",
                                "text": "done\nDEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/owner/repo:sha-abcdef0\",\"message\":\"Deployment image pushed to GHCR\",\"error\":null}"
                            }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "succeeded");
        assert_eq!(
            status.image.as_deref(),
            Some("ghcr.io/owner/repo:sha-abcdef0")
        );
    }

    #[test]
    fn deployment_status_reports_running_for_active_thread_without_result() {
        let thread = json!({
            "thread": {
                "status": { "type": "active" },
                "turns": []
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "running");
        assert_eq!(status.image, None);
        assert_eq!(status.error, None);
    }

    #[test]
    fn deployment_status_reports_failed_for_completed_thread_without_result() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            { "type": "agentMessage", "text": "no structured result" }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "failed");
        assert!(status.error.as_deref().unwrap_or("").contains("not found"));
    }

    #[test]
    fn deployment_status_requires_image_on_success() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "agentMessage",
                                "text": "DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":null,\"message\":\"done\",\"error\":null}"
                            }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "failed");
        assert!(status.error.as_deref().unwrap_or("").contains("GHCR image"));
    }

    #[test]
    fn deployment_status_requires_ghcr_image_on_success() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "agentMessage",
                                "text": "DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"docker.io/owner/repo:sha-abcdef0\",\"message\":\"done\",\"error\":null}"
                            }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "failed");
        assert!(status.error.as_deref().unwrap_or("").contains("GHCR image"));
    }

    #[test]
    fn deployment_status_treats_latest_invalid_result_as_failed() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "agentMessage",
                                "text": "DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/owner/repo:sha-old\",\"message\":\"old\",\"error\":null}"
                            }
                        ]
                    },
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "agentMessage",
                                "text": "DEPLOYMENT_RESULT: not-json"
                            }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "failed");
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to parse")
        );
    }

    #[test]
    fn deployment_status_requires_result_line_to_be_final() {
        let thread = json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    {
                        "status": "completed",
                        "items": [
                            {
                                "type": "agentMessage",
                                "text": "DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/owner/repo:sha-abcdef0\",\"message\":\"done\",\"error\":null}\nextra"
                            }
                        ]
                    }
                ]
            }
        });

        let status = brain_deployment_status_from_thread("thread-1", &thread);

        assert_eq!(status.status, "failed");
        assert!(status.error.as_deref().unwrap_or("").contains("final"));
    }
}
