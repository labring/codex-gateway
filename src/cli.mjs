import process from "node:process";
import { CodexAppServerBridge } from "./codex-app-server.mjs";
import { maybeLoginWithApiKey } from "./codex-runtime.mjs";

const DEFAULT_PROMPT =
  "Reply with exactly the single word pong. Do not call tools. Do not read files. Do not run commands. Do not use markdown.";

const prompt = process.argv.slice(2).join(" ").trim() || DEFAULT_PROMPT;
const bridge = new CodexAppServerBridge();

bridge.on("notification", (message) => {
  const { method, params = {} } = message;
  const itemType = params.item?.type;
  const suffix = itemType ? ` ${itemType}` : params.turn?.status ? ` ${params.turn.status}` : "";
  console.log(`[notify] ${method}${suffix}`);
});

bridge.on("warning", (warning) => {
  console.warn(`[warn] ${warning.message}`);
  if (warning.detail) {
    console.warn(warning.detail);
  }
});

async function main() {
  await maybeLoginWithApiKey();
  const state = await bridge.start();

  console.log(`Starting ${bridge.codexBin} app-server from ${state.cwd}`);
  console.log("Initialized app-server");
  console.log(
    `Runtime: ${state.runtime.platformFamily ?? "unknown"} / ${state.runtime.platformOs ?? "unknown"}`,
  );
  console.log(
    `Account: ${state.account.summary} | requiresOpenaiAuth=${state.account.requiresOpenaiAuth}`,
  );
  console.log(`Selected model: ${state.selectedModel}`);
  console.log(`Thread: ${state.threadId}`);
  console.log(`Prompt: ${prompt}`);

  await bridge.sendPrompt(prompt);
  await bridge.waitForTurnCompletion();

  console.log("\nFinal agent text:\n");
  console.log(bridge.getLatestAssistantText());

  await bridge.stop();
}

main().catch(async (error) => {
  console.error("[fatal] Demo failed");
  console.error(error);
  await bridge.stop();
  process.exit(1);
});
