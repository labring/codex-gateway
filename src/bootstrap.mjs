import { maybeLoginWithApiKey } from "./codex-runtime.mjs";

await maybeLoginWithApiKey();
await import("./server.mjs");
