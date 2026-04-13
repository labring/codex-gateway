import process from "node:process";

export const ENV_NAMES = {
  host: ["CODEX_GATEWAY_HOST"],
  port: ["CODEX_GATEWAY_PORT"],
  bridgeCwd: ["CODEX_GATEWAY_CWD"],
  codexBin: ["CODEX_GATEWAY_CODEX_BIN"],
  debug: ["CODEX_GATEWAY_DEBUG"],
  defaultModel: ["CODEX_GATEWAY_MODEL"],
  maxSessions: ["CODEX_GATEWAY_MAX_SESSIONS"],
  sessionTtlMs: ["CODEX_GATEWAY_SESSION_TTL_MS"],
  sessionSweepIntervalMs: ["CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS"],
  openaiApiKey: ["CODEX_GATEWAY_OPENAI_API_KEY"],
  openaiBaseUrl: ["CODEX_GATEWAY_OPENAI_BASE_URL"],
  codexHome: ["CODEX_GATEWAY_CODEX_HOME"],
};

export function readEnv(names) {
  for (const name of names) {
    const value = process.env[name];
    if (typeof value !== "string") {
      continue;
    }

    const trimmed = value.trim();
    if (trimmed) {
      return trimmed;
    }
  }

  return null;
}

export function readBooleanFlag(names) {
  return readEnv(names) === "1";
}

export function buildCodexChildEnv() {
  const env = { ...process.env };
  const codexHome = readEnv(ENV_NAMES.codexHome);
  const defaultModel = readEnv(ENV_NAMES.defaultModel);
  const openaiApiKey = readEnv(ENV_NAMES.openaiApiKey);
  const openaiBaseUrl = readEnv(ENV_NAMES.openaiBaseUrl);

  if (codexHome) {
    env.CODEX_HOME = codexHome;
  }

  if (defaultModel) {
    env.CODEX_MODEL = defaultModel;
  }

  if (openaiApiKey) {
    env.OPENAI_API_KEY = openaiApiKey;
  }

  if (openaiBaseUrl) {
    env.CODEX_OPENAI_BASE_URL = openaiBaseUrl;
    env.OPENAI_BASE_URL = openaiBaseUrl;
  }

  return env;
}
