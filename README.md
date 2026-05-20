# Codex Gateway

Chinese version: [README_zh.md](./README_zh.md)

Codex Gateway is a Rust HTTP/SSE gateway for running isolated Codex sessions behind a small API and browser UI.

Generated Qoder documentation may exist under `.qoder/`; it is generated output and should not be edited by hand or committed as source documentation. This README is the maintained human entry point.

## Runtime Shape

All Gateway sessions use the embedded runtime:

1. A client creates a session through the Rust gateway.
2. The session owns a `CodexAppServerBridge`.
3. The bridge starts and manages one `codex app-server` subprocess over stdio.
4. App-server notifications are folded into session state and streamed to the client over SSE.

Brain deployment tasks use the same embedded runtime, but expose a polling-only task API instead of the interactive session API. Gateway does not create or manage Devbox runtimes for deployment requests.

If deployment work runs in Devbox, an external system is responsible for creating the Devbox and starting this gateway inside it before calling the Brain Deployment API.

## Brain Deployment API

`POST /api/brain/deployments` is a Brain application API. It is not intended to describe a general deployment product surface.

The endpoint creates a local embedded Codex task that installs the deployment skill if needed, builds the repository image, pushes it to GHCR, generates a Sealos template, and reports a machine-readable deployment result containing both the image reference and template content. It does not expose intermediate Codex output or accept follow-up user turns.

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
- `POST /api/brain/deployments`
- `GET /api/brain/deployments/:threadId`

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

Devbox lifecycle is external to this gateway. If the gateway is running in Devbox, configure the process with the normal gateway settings above.

## Verification

```bash
cargo fmt --check
cargo test
```
