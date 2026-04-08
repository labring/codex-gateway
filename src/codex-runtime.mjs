import { spawn } from "node:child_process";
import process from "node:process";

function readEnv(name) {
  const value = process.env[name];
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function tomlString(value) {
  return JSON.stringify(value);
}

export function getOpenaiBaseUrl() {
  return readEnv("CODEX_OPENAI_BASE_URL") ?? readEnv("OPENAI_BASE_URL");
}

export function getCodexConfigArgs() {
  const args = [];
  const baseUrl = getOpenaiBaseUrl();
  const hasApiKey = Boolean(readEnv("OPENAI_API_KEY"));

  if (baseUrl) {
    args.push("-c", `openai_base_url=${tomlString(baseUrl)}`);
  }

  if (baseUrl || hasApiKey) {
    args.push("-c", 'forced_login_method="api"');
  }

  return args;
}

export async function maybeLoginWithApiKey({
  codexBin = process.env.CODEX_BIN || "codex",
} = {}) {
  const apiKey = readEnv("OPENAI_API_KEY");
  if (!apiKey) {
    return false;
  }

  const baseUrl = getOpenaiBaseUrl();
  const args = ["login", ...getCodexConfigArgs(), "--with-api-key"];

  console.log(
    baseUrl
      ? `Initializing Codex auth from OPENAI_API_KEY with base URL override ${baseUrl}`
      : "Initializing Codex auth from OPENAI_API_KEY",
  );

  await new Promise((resolve, reject) => {
    const child = spawn(codexBin, args, {
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
          `${codexBin} login failed while reading OPENAI_API_KEY (code=${code}, signal=${signal})`,
        ),
      );
    });

    child.stdin.end(`${apiKey}\n`);
  });

  return true;
}
