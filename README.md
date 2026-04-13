# Codex Gateway

Chinese version: [README_zh.md](./README_zh.md)

This repository is a minimal multi-session gateway for verifying that `codex app-server` can be exposed as a small HTTP/SSE service.

The current shape is:

1. external clients call a Rust HTTP API
2. the Rust service creates one Codex bridge per session
3. each bridge spawns its own local `codex app-server` child process over `stdio`
4. streamed notifications are forwarded back to that client over SSE

Official references used while building this:

- [Codex App Server](https://developers.openai.com/codex/app-server/)
- [Codex CLI quickstart](https://developers.openai.com/codex/quickstart/#setup)
- [Codex configuration reference](https://developers.openai.com/codex/config-reference/)
- [Codex CI/CD auth](https://developers.openai.com/codex/auth/ci-cd-auth)

## What is in here

- `rust-src/main.rs`: Rust HTTP server with session APIs, SSE streams, health endpoints, and static file serving
- `rust-src/bridge.rs`: reusable bridge for `initialize`, `account/read`, `model/list`, `thread/start`, `turn/start`, and notification handling
- `rust-src/runtime.rs`: shared runtime helpers for API-key login and `openai_base_url` overrides
- `rust-src/session_manager.rs`: multi-session lifecycle manager for bridges and TTL cleanup
- `rust-src/cli.rs`: one-shot CLI smoke test for a single bridge
- `src/*.mjs`: previous Node implementation retained temporarily as migration reference
- `public/index.html`: minimal browser UI
- `public/app.js`: browser behavior that creates its own API session and listens to its own SSE stream
- `public/styles.css`: intentionally simple UI styling
- `Dockerfile`: multi-stage image that builds the Rust gateway and installs the Codex CLI on Linux

## Runtime model

This is no longer a single shared in-memory conversation.

- `POST /api/sessions` creates a new session
- each session owns one `CodexAppServerBridge`
- each bridge owns one `codex app-server` subprocess
- all `/state`, `/events`, `/turn`, and `/thread/new` calls are scoped to one session id
- sessions expire after an idle TTL and are also removable explicitly with `DELETE /api/sessions/:id`

That makes the service usable by multiple callers without sharing one thread or transcript.

## HTTP API

### Health

- `GET /healthz`
- `GET /readyz`

### Sessions

- `POST /api/sessions`
  - body: `{ "model": "optional-model-id" }`
  - returns: `{ ok, sessionId, session, state }`
- `GET /api/sessions/:id/state`
  - returns the latest session metadata plus the current bridge state snapshot
- `GET /api/sessions/:id/events`
  - SSE stream for that session only
- `POST /api/sessions/:id/turn`
  - body: `{ "prompt": "..." }`
- `POST /api/sessions/:id/thread/new`
  - body: `{ "model": "optional-model-id" }`
- `DELETE /api/sessions/:id`
  - closes the session and its child process

### Important behavior

- approval requests are still auto-declined
- unsupported server-initiated requests are rejected
- session state is in memory only
- gateway auth is optional and is enabled only when `CODEX_GATEWAY_JWT_SECRET` is set
- one session can only have one active turn at a time

## Local usage

### Web UI

Start the local server:

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
CODEX_GATEWAY_JWT_SECRET=replace-with-your-hs256-secret \
cargo run --bin codex-gateway
```

Then open:

```text
http://127.0.0.1:1317
```

The page creates a fresh session automatically, subscribes to its own SSE stream, and tears the session down on tab close when possible. When JWT auth is enabled, paste a bearer token into the `Auth` panel before using the page.

### CLI smoke test

Run the one-shot harness:

```bash
cargo run --bin codex-gateway-cli --
```

Or with a custom prompt:

```bash
cargo run --bin codex-gateway-cli -- "Reply with exactly the single word ready."
```

## Verification

If you want to verify the project manually, the shortest path is:

1. Start the service with `cargo run --bin codex-gateway`.
2. Check `http://127.0.0.1:1317/healthz`.
3. Check `http://127.0.0.1:1317/readyz`.
4. Open `http://127.0.0.1:1317` and wait for the page status to become `ready`.
5. Send `Reply with exactly the single word ready. Do not call tools.` from the page.
6. Confirm that the transcript shows `ready`.

If you want to verify the API directly instead of the page:

Create a session:

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

Send a turn:

```bash
curl -X POST http://127.0.0.1:1317/api/sessions/<SESSION_ID>/turn \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"Reply with exactly the single word ready. Do not call tools."}'
```

Read the latest state:

```bash
curl http://127.0.0.1:1317/api/sessions/<SESSION_ID>/state
```

If the transcript contains `ready`, the gateway, bridge, and `codex app-server` handshake are all working.

## Environment variables

Gateway-owned settings use the `CODEX_GATEWAY_` prefix for better discoverability.

- `CODEX_GATEWAY_HOST`: bind address for the Rust server. Defaults to `0.0.0.0`.
- `CODEX_GATEWAY_PORT`: bind port. Defaults to `1317`.
- `CODEX_GATEWAY_CWD`: working directory passed to `thread/start`. Defaults to the repository root.
- `CODEX_GATEWAY_CODEX_BIN`: path to the `codex` executable if it is not on `PATH`.
- `CODEX_GATEWAY_MODEL`: preferred default model for new bridges.
- `CODEX_GATEWAY_OPENAI_API_KEY`: API key used at startup to run `codex login --with-api-key`.
- `CODEX_GATEWAY_OPENAI_BASE_URL`: upstream OpenAI-compatible base URL. When set, the gateway configures Codex to use a custom provider with `supports_websockets = false`.
- `CODEX_GATEWAY_MAX_SESSIONS`: maximum live sessions. Defaults to `12`.
- `CODEX_GATEWAY_SESSION_TTL_MS`: idle session TTL. Defaults to `1800000`.
- `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS`: cleanup sweep interval. Defaults to `60000`.
- `CODEX_GATEWAY_CODEX_HOME`: Codex runtime home for auth cache, logs, history, and config. In Docker this defaults to `/codex-home`.
- `CODEX_GATEWAY_DEBUG`: enables raw bridge message debugging when set to `1`.
- `CODEX_GATEWAY_JWT_SECRET`: optional HS256 JWT secret. When set, the gateway requires a valid bearer token for all routes except `/healthz` and `/readyz`.

## Docker

The container image builds the Rust gateway binary, then installs the Codex CLI on Linux with `npm install -g @openai/codex`, which matches the official Codex CLI quickstart.

Build the image:

```bash
docker build -t codex-gateway .
```

Run it:

```bash
docker run --rm \
  -p 1317:1317 \
  -e CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
  -e CODEX_GATEWAY_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
  -e CODEX_GATEWAY_JWT_SECRET=replace-with-your-hs256-secret \
  -e CODEX_GATEWAY_HOST=0.0.0.0 \
  -e CODEX_GATEWAY_PORT=1317 \
  -e CODEX_GATEWAY_MAX_SESSIONS=8 \
  codex-gateway
```

Notes:

- if `CODEX_GATEWAY_OPENAI_API_KEY` is set, the container runs `codex login --with-api-key` automatically before starting the gateway
- `CODEX_GATEWAY_OPENAI_BASE_URL` is the preferred way to point Codex at a third-party OpenAI-compatible endpoint; the gateway maps it to a custom Codex provider instead of the built-in `openai` provider
- if `CODEX_GATEWAY_JWT_SECRET` is set, clients must send `Authorization: Bearer <jwt>` on normal HTTP requests; the built-in Web UI also supports pasting the token into the sidebar
- you do not need to mount `CODEX_GATEWAY_CODEX_HOME` for normal API-key-based startup; mount it only if you want Codex state to persist across container restarts
- if you want Codex to operate on another workspace inside the container, set `CODEX_GATEWAY_CWD` and mount that path too
- this is a PoC deployment shape, not a hardened public service
- after the container starts, use the same health/API/Web UI verification flow described above

## GitHub Container Registry

GitHub Actions can publish this image to GHCR after pushes to `main` and version tags such as `v0.2.0`.

Published tags:

- `ghcr.io/che-zhu/codex-gateway:main` for the latest `main` branch image
- `ghcr.io/che-zhu/codex-gateway:sha-<commit>` for each published commit
- `ghcr.io/che-zhu/codex-gateway:v0.2.0`, `0.2.0`, `0.2`, `0`, and `latest` when pushing a version tag

Pull the current `main` image:

```bash
docker pull ghcr.io/che-zhu/codex-gateway:main
```

Run it the same way as the local image:

```bash
docker run --rm \
  -p 1317:1317 \
  -e CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
  -e CODEX_GATEWAY_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
  -e CODEX_GATEWAY_HOST=0.0.0.0 \
  -e CODEX_GATEWAY_PORT=1317 \
  -e CODEX_GATEWAY_MAX_SESSIONS=8 \
  ghcr.io/che-zhu/codex-gateway:main
```

If the package is private, authenticate to GHCR before pulling it.

## Current limitations

- no built-in rate limiting
- no durable session persistence
- approval UI is intentionally absent
- each live session consumes a `codex app-server` subprocess
- browser clients reconnect with SSE, but session ownership is not persisted across page reloads unless the caller stores the session id
