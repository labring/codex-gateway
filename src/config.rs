use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
}

/// Langfuse export settings plus the trace identity injected by Brain
/// through the Devbox environment.
#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub host: String,
    pub public_key: String,
    pub secret_key: String,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub cwd: PathBuf,
    pub codex_bin: String,
    pub codex_home: Option<String>,
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub default_model: Option<String>,
    pub debug: bool,
    pub max_sessions: usize,
    pub session_ttl: Duration,
    pub session_sweep_interval: Duration,
    pub client_info: ClientInfo,
    pub auth: Option<AuthConfig>,
    pub langfuse: Option<LangfuseConfig>,
}

impl AppConfig {
    pub fn from_env(root_dir: PathBuf) -> Self {
        Self {
            host: read_env("CODEX_GATEWAY_HOST").unwrap_or_else(|| "0.0.0.0".to_string()),
            port: read_parsed("CODEX_GATEWAY_PORT").unwrap_or(1317),
            cwd: read_env("CODEX_GATEWAY_CWD")
                .map(PathBuf::from)
                .unwrap_or(root_dir),
            codex_bin: read_env("CODEX_GATEWAY_CODEX_BIN").unwrap_or_else(|| "codex".to_string()),
            codex_home: read_env("CODEX_GATEWAY_CODEX_HOME"),
            openai_api_key: read_env("CODEX_GATEWAY_OPENAI_API_KEY"),
            openai_base_url: read_env("CODEX_GATEWAY_OPENAI_BASE_URL"),
            default_model: read_env("CODEX_GATEWAY_MODEL"),
            debug: read_env("CODEX_GATEWAY_DEBUG").as_deref() == Some("1"),
            max_sessions: read_positive("CODEX_GATEWAY_MAX_SESSIONS").unwrap_or(12) as usize,
            session_ttl: Duration::from_millis(
                read_positive("CODEX_GATEWAY_SESSION_TTL_MS").unwrap_or(30 * 60 * 1000),
            ),
            session_sweep_interval: Duration::from_millis(
                read_positive("CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS").unwrap_or(60 * 1000),
            ),
            client_info: ClientInfo {
                name: "codex_gateway".to_string(),
                title: "Codex Gateway".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            auth: read_env("CODEX_GATEWAY_JWT_SECRET").map(|jwt_secret| AuthConfig { jwt_secret }),
            langfuse: langfuse_from_env(),
        }
    }

    /// Environment passed to every `codex` child process. Values come from
    /// this config only, so there is a single source of truth for them.
    pub fn codex_child_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(value) = &self.codex_home {
            env.push(("CODEX_HOME".to_string(), value.clone()));
        }
        if let Some(value) = &self.default_model {
            env.push(("CODEX_MODEL".to_string(), value.clone()));
        }
        if let Some(value) = &self.openai_api_key {
            env.push(("OPENAI_API_KEY".to_string(), value.clone()));
        }
        if let Some(value) = &self.openai_base_url {
            env.push(("CODEX_OPENAI_BASE_URL".to_string(), value.clone()));
            env.push(("OPENAI_BASE_URL".to_string(), value.clone()));
        }
        env
    }

    /// `-c key=value` overrides shared by `codex login` and `codex app-server`.
    pub fn codex_config_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(base_url) = self.openai_base_url.as_deref() {
            args.extend(custom_provider_args(base_url));
        }
        if self.openai_base_url.is_some() || self.openai_api_key.is_some() {
            args.push("-c".to_string());
            args.push(r#"forced_login_method="api""#.to_string());
        }
        args
    }

    /// Additional overrides for `codex app-server`: the gateway runs inside a
    /// disposable sandbox, so commands execute without approval prompts.
    pub fn codex_app_server_args(&self) -> Vec<String> {
        let mut args = self.codex_config_args();
        args.push("-c".to_string());
        args.push(format!(
            "sandbox_mode={}",
            toml_string("danger-full-access")
        ));
        args.push("-c".to_string());
        args.push(format!("approval_policy={}", toml_string("never")));
        args
    }
}

fn langfuse_from_env() -> Option<LangfuseConfig> {
    let public_key = read_env("LANGFUSE_PUBLIC_KEY")?;
    let secret_key = read_env("LANGFUSE_SECRET_KEY")?;
    Some(LangfuseConfig {
        host: read_env("LANGFUSE_HOST").unwrap_or_else(|| "https://cloud.langfuse.com".to_string()),
        public_key,
        secret_key,
        user_id: read_env("SEALAI_NAMESPACE"),
        session_id: read_env("SEALAI_DEPLOY_TASK_ID"),
        project_id: read_env("SEALAI_PROJECT_ID"),
    })
}

fn custom_provider_args(base_url: &str) -> Vec<String> {
    const PROVIDER_ID: &str = "OpenAI";
    let provider_path = format!("model_providers.{PROVIDER_ID}");

    vec![
        "-c".to_string(),
        format!("model_provider={}", toml_string(PROVIDER_ID)),
        "-c".to_string(),
        format!("{provider_path}.name={}", toml_string(PROVIDER_ID)),
        "-c".to_string(),
        format!("{provider_path}.base_url={}", toml_string(base_url)),
        "-c".to_string(),
        format!("{provider_path}.wire_api={}", toml_string("responses")),
        "-c".to_string(),
        format!("{provider_path}.requires_openai_auth=true"),
        "-c".to_string(),
        format!("{provider_path}.supports_websockets=false"),
    ]
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

fn read_env(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_parsed<T: std::str::FromStr>(name: &str) -> Option<T> {
    read_env(name)?.parse().ok()
}

fn read_positive(name: &str) -> Option<u64> {
    read_parsed::<u64>(name).filter(|value| *value > 0)
}
