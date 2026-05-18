# Codex Gateway

Chinese version: [README_zh.md](./README_zh.md)

Codex Gateway is a Rust HTTP/SSE gateway for running isolated Codex sessions behind a small API and browser UI.

Generated Qoder documentation may exist under `.qoder/`; it is generated output and should not be edited by hand or committed as source documentation. This README is the maintained human entry point.

## Runtime Shape

The default runtime is `embedded`:

1. A client creates a session through the Rust gateway.
2. The session owns a `CodexAppServerBridge`.
3. The bridge starts and manages one `codex app-server` subprocess over stdio.
4. App-server notifications are folded into session state and streamed to the client over SSE.

The optional `devbox` runtime is a remote execution backend:

1. The outer gateway creates a Devbox runtime.
2. It waits for the gateway inside Devbox to become ready.
3. It creates a remote session in that inner gateway.
4. The inner gateway uses the `embedded` runtime to run `codex app-server`.

`devbox` is runtime infrastructure, not a product mode.

## Brain Deployment API

`POST /api/deployments` is a Brain application reserved API. It is not intended to describe a general deployment product surface.

The endpoint creates a Codex task that deploys a repository and reports a machine-readable deployment result. When the active session runtime is Devbox-backed, Gateway bootstraps the Devbox runtime before starting the Brain deployment task.

## HTTP API

- `GET /healthz`
- `GET /readyz`
- `POST /api/sessions`
- `GET /api/sessions/:id/state`
- `GET /api/sessions/:id/events`
- `POST /api/sessions/:id/turn`
- `POST /api/sessions/:id/turn/interrupt`
- `POST /api/sessions/:id/thread/new`
- `POST /api/sessions/:id/thread/resume`
- `DELETE /api/sessions/:id`
- `GET /api/threads`
- `GET /api/threads/:threadId`
- `POST /api/deployments`
- `GET /api/deployments/:threadId`

Legacy single-session routes such as `/api/state`, `/api/events`, `/api/turn`, and `/api/thread/new` are removed and return `410 Gone`.

## Local Usage

Start the gateway:

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
CODEX_GATEWAY_JWT_SECRET=replace-with-your-hs256-secret \
cargo run --bin codex-gateway
```

Open:

```text
http://127.0.0.1:1317
```

Quick API smoke test:

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

## Configuration

Gateway-owned settings use the `CODEX_GATEWAY_` prefix.

- `CODEX_GATEWAY_HOST`: bind address. Defaults to `0.0.0.0`.
- `CODEX_GATEWAY_PORT`: bind port. Defaults to `1317`.
- `CODEX_GATEWAY_CWD`: working directory passed to `thread/start`.
- `CODEX_GATEWAY_CODEX_BIN`: path to the `codex` executable.
- `CODEX_GATEWAY_MODEL`: preferred default model.
- `CODEX_GATEWAY_OPENAI_API_KEY`: API key used at startup for `codex login --with-api-key`.
- `CODEX_GATEWAY_OPENAI_BASE_URL`: upstream OpenAI-compatible base URL.
- `CODEX_GATEWAY_JWT_SECRET`: optional HS256 JWT secret.
- `CODEX_GATEWAY_MAX_SESSIONS`: maximum live sessions. Defaults to `12`.
- `CODEX_GATEWAY_SESSION_TTL_MS`: idle session TTL. Defaults to `1800000`.
- `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS`: cleanup sweep interval. Defaults to `60000`.
- `CODEX_GATEWAY_SESSION_RUNTIME`: session runtime backend. Defaults to `embedded`. Supported values are `embedded` and `devbox`.
- `CODEX_GATEWAY_MAX_DEPLOYMENTS`: maximum active Brain deployment tasks. Defaults to `4`.
- `CODEX_GATEWAY_DEPLOYMENT_TIMEOUT_MS`: Brain deployment timeout and session keepalive window. Defaults to `3600000`.

Devbox-related settings are only used when the runtime is `devbox`:

- `CODEX_GATEWAY_DEVBOX_BASE_URL`
- `CODEX_GATEWAY_DEVBOX_TOKEN`
- `CODEX_GATEWAY_DEVBOX_JWT_SIGNING_KEY`
- `CODEX_GATEWAY_DEVBOX_NAMESPACE`
- `CODEX_GATEWAY_DEVBOX_RUNTIME_IMAGE`
- `CODEX_GATEWAY_DEVBOX_ARCHIVE_AFTER_PAUSE_TIME`
- `CODEX_GATEWAY_DEVBOX_WAIT_TIMEOUT_SECONDS`
- `CODEX_GATEWAY_DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS`
- `CODEX_GATEWAY_DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS`

## Verification

```bash
cargo fmt --check
cargo test
```
