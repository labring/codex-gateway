import { spawn } from "node:child_process";
import { buildCodexChildEnv, ENV_NAMES, readEnv } from "./env-config.mjs";
const CUSTOM_PROVIDER_ID = "OpenAI";

function tomlString(value) {
  return JSON.stringify(value);
}

export function getOpenaiBaseUrl() {
  return readEnv(ENV_NAMES.openaiBaseUrl);
}

function getCustomProviderConfigArgs(baseUrl) {
  const providerPath = `model_providers.${CUSTOM_PROVIDER_ID}`;

  return [
    "-c",
    `model_provider=${tomlString(CUSTOM_PROVIDER_ID)}`,
    "-c",
    `${providerPath}.name=${tomlString(CUSTOM_PROVIDER_ID)}`,
    "-c",
    `${providerPath}.base_url=${tomlString(baseUrl)}`,
    "-c",
    `${providerPath}.wire_api=${tomlString("responses")}`,
    "-c",
    `${providerPath}.requires_openai_auth=true`,
    "-c",
    `${providerPath}.supports_websockets=false`,
  ];
}

export function getCodexConfigArgs() {
  const args = [];
  const baseUrl = getOpenaiBaseUrl();
  const hasApiKey = Boolean(readEnv(ENV_NAMES.openaiApiKey));

  if (baseUrl) {
    args.push(...getCustomProviderConfigArgs(baseUrl));
  }

  if (baseUrl || hasApiKey) {
    args.push("-c", 'forced_login_method="api"');
  }

  return args;
}

export function getCodexAppServerConfigArgs() {
  return [
    ...getCodexConfigArgs(),
    "-c",
    `sandbox_mode=${tomlString("danger-full-access")}`,
    "-c",
    `approval_policy=${tomlString("never")}`,
  ];
}

export async function maybeLoginWithApiKey({
  codexBin = readEnv(ENV_NAMES.codexBin) || "codex",
} = {}) {
  const apiKey = readEnv(ENV_NAMES.openaiApiKey);
  if (!apiKey) {
    return false;
  }

  const baseUrl = getOpenaiBaseUrl();
  const args = ["login", ...getCodexConfigArgs(), "--with-api-key"];

  console.log(
    baseUrl
      ? `Initializing Codex auth from configured OpenAI API key with base URL override ${baseUrl}`
      : "Initializing Codex auth from configured OpenAI API key",
  );

  await new Promise((resolve, reject) => {
    const child = spawn(codexBin, args, {
      env: buildCodexChildEnv(),
      stdio: ["pipe", "inherit", "inherit"],
    });

    child.on("error", (error) => {
      reject(new Error(`Failed to start ${codexBin} login: ${error.message}`));
    });

    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }

      reject(
        new Error(
          `${codexBin} login failed while reading the configured OpenAI API key (code=${code}, signal=${signal})`,
        ),
      );
    });

    child.stdin.end(`${apiKey}\n`);
  });

  return true;
}
