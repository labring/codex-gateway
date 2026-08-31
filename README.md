# Codex Gateway

Chinese version: [README_zh.md](./README_zh.md)

HTTP/SSE gateway for Codex. Each session starts one `codex app-server` child process over stdio; the gateway runs turns against it, streams its events to the client over SSE, and optionally records every user/agent interaction to [Langfuse](https://langfuse.com/).

Sessions are separate processes that share the gateway working directory. The gateway starts app-server with `sandbox_mode=danger-full-access` and `approval_policy=never`, and answers approval requests automatically: it is built to run inside a disposable sandbox (a Sealos Devbox).

## Run locally

You need Rust 1.94.1 (`rust-toolchain.toml`) and `codex` on `PATH`.

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
cargo run --bin codex-gateway
```

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

Auth is off unless you set `CODEX_GATEWAY_JWT_SECRET`. Then every route except `/healthz` and `/readyz` needs `Authorization: Bearer <jwt>`. The JWT must be HS256 and include `exp`. EventSource cannot set headers, so SSE accepts `?access_token=<jwt>`.

`codex-gateway-cli` is a smoke tool: it runs one prompt against a fresh app-server and prints the final agent text.

## Docker

Images are `linux/amd64` at `ghcr.io/labring/codex-gateway`.

```bash
docker run --rm -p 1317:1317 \
  -e CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
  -e CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
  ghcr.io/labring/codex-gateway:main
```

The image sets `CODEX_GATEWAY_MAX_SESSIONS=8`. Without that env, the process default is `12`.

## HTTP API

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/healthz` | Liveness |
| `GET` | `/readyz` | Process is up; includes `activeSessions` |
| `POST` | `/api/sessions` | Create a session |
| `GET` | `/api/sessions/:id/state` | Session snapshot |
| `GET` | `/api/sessions/:id/events` | SSE event stream |
| `POST` | `/api/sessions/:id/turn` | Send a prompt |
| `POST` | `/api/sessions/:id/turn/interrupt` | Stop the active turn |
| `DELETE` | `/api/sessions/:id` | Close the session |

### Sessions

`POST /api/sessions`

```json
{
  "model": "gpt-5",
  "resumeThreadId": "thread-1"
}
```

Both fields are optional; an empty body starts a new thread with the default model. `threadId` is accepted as an alias for `resumeThreadId`.

```json
{
  "ok": true,
  "sessionId": "...",
  "session": {},
  "state": {}
}
```

`state` is `{ ready, cwd, startedAt, selectedModel, threadId, currentTurnId, activeTurn, lastTurnStatus }`. Thread history is not mirrored by the gateway; read it from Codex or from Langfuse.

`POST /api/sessions/:id/turn` body: `{ "prompt": "..." }`. Returns `409` while a turn is active.

### SSE events

`GET /api/sessions/:id/events` emits `session` and `state` on connect, then `state`, `notification` (raw app-server JSON-RPC notifications), `server-request`, `warning`, `session-closed`, and — with `CODEX_GATEWAY_DEBUG=1` — `raw` events.

## Langfuse tracing

Set `LANGFUSE_PUBLIC_KEY` and `LANGFUSE_SECRET_KEY` to record every user/agent interaction. Spans are sent over OTLP (HTTP/protobuf) to `{LANGFUSE_HOST}/api/public/otel/v1/traces`, batched on a background thread; a Langfuse outage can only drop telemetry, never block requests.

Each turn becomes one trace: the root span carries the prompt as input, the final agent message as output, and token usage. Completed codex items become child observations (command executions with their output, file changes, MCP tool calls, agent messages as generations). App-server errors, gateway warnings, and auto-answered approval requests become events. Known credential shapes (GitHub tokens, API keys, bearer tokens) are redacted before export, and payloads are capped at 8 KiB.

Trace identity comes from the environment Brain injects when it creates the Devbox: `langfuse.user.id` = `SEALAI_NAMESPACE`, `langfuse.session.id` = `SEALAI_DEPLOY_TASK_ID` (falling back to the codex thread id), and `SEALAI_PROJECT_ID` lands in trace metadata — so a failing deploy task can be looked up in Langfuse by its task id.

## Configuration

| Variable | Meaning | Default |
| --- | --- | --- |
| `CODEX_GATEWAY_HOST` | Bind address | `0.0.0.0` |
| `CODEX_GATEWAY_PORT` | Bind port | `1317` |
| `CODEX_GATEWAY_CWD` | Working directory for `thread/start` | process cwd |
| `CODEX_GATEWAY_CODEX_BIN` | `codex` executable | `codex` |
| `CODEX_GATEWAY_CODEX_HOME` | Passed to the child as `CODEX_HOME` | unset |
| `CODEX_GATEWAY_MODEL` | Default model | unset |
| `CODEX_GATEWAY_DEBUG` | Set to `1` to log and stream raw app-server lines | off |
| `CODEX_GATEWAY_OPENAI_API_KEY` | Used at startup for `codex login --with-api-key` | unset |
| `CODEX_GATEWAY_OPENAI_BASE_URL` | Upstream OpenAI-compatible base URL | unset |
| `CODEX_GATEWAY_JWT_SECRET` | HS256 secret; unset means no auth | unset |
| `CODEX_GATEWAY_MAX_SESSIONS` | Max live sessions | `12` |
| `CODEX_GATEWAY_SESSION_TTL_MS` | Idle session TTL | `1800000` |
| `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS` | Session cleanup interval | `60000` |
| `LANGFUSE_PUBLIC_KEY` | Langfuse project public key; tracing is off without it | unset |
| `LANGFUSE_SECRET_KEY` | Langfuse project secret key | unset |
| `LANGFUSE_HOST` | Langfuse base URL | `https://cloud.langfuse.com` |
| `SEALAI_NAMESPACE` | Langfuse `user.id` | unset |
| `SEALAI_DEPLOY_TASK_ID` | Langfuse `session.id` | codex thread id |
| `SEALAI_PROJECT_ID` | Trace metadata `projectId` | unset |

## Checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Integration tests run the real gateway binary against a fake `codex app-server` and a fake Langfuse OTLP endpoint.
