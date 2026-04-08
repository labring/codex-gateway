# Codex Gateway Minimal

Chinese version: [README_zh.md](./README_zh.md)

This repository is a minimal multi-session gateway for verifying that `codex app-server` can be exposed as a small HTTP/SSE service.

The repository folder is still named `codex-sandbox`, and the npm package name is still the older `codex-app-server-minimal`. The runtime shape documented here is the current source of truth.

The current shape is:

1. external clients call a Node HTTP API
2. the Node service creates one Codex bridge per session
3. each bridge spawns its own local `codex app-server` child process over `stdio`
4. streamed notifications are forwarded back to that client over SSE

Official references used while building this:

- [Codex App Server](https://developers.openai.com/codex/app-server/)
- [Codex CLI quickstart](https://developers.openai.com/codex/quickstart/#setup)

## What is in here

- `src/codex-app-server.mjs`: reusable bridge for `initialize`, `account/read`, `model/list`, `thread/start`, `turn/start`, and notification handling
- `src/session-manager.mjs`: multi-session lifecycle manager for bridges, TTL cleanup, and session-scoped event forwarding
- `src/server.mjs`: local/public HTTP server with session APIs, SSE streams, health endpoints, and the demo UI
- `src/cli.mjs`: one-shot CLI smoke test for a single bridge
- `public/index.html`: minimal browser UI
- `public/app.js`: browser behavior that creates its own API session and listens to its own SSE stream
- `public/styles.css`: intentionally simple UI styling
- `Dockerfile`: single-container runtime image that installs the Codex CLI on Linux

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
- there is no auth layer in this PoC
- one session can only have one active turn at a time

## Local usage

### Web UI

Start the local server:

```bash
npm start
```

Then open:

```text
http://127.0.0.1:3000
```

The page creates a fresh session automatically, subscribes to its own SSE stream, and tears the session down on tab close when possible.

### CLI smoke test

Run the one-shot harness:

```bash
npm run cli
```

Or with a custom prompt:

```bash
npm run cli -- "Reply with exactly the single word ready."
```

## Verification

If you want to verify the project manually, the shortest path is:

1. Start the service with `npm start`.
2. Check `http://127.0.0.1:3000/healthz`.
3. Check `http://127.0.0.1:3000/readyz`.
4. Open `http://127.0.0.1:3000` and wait for the page status to become `ready`.
5. Send `Reply with exactly the single word ready. Do not call tools.` from the page.
6. Confirm that the transcript shows `ready`.

If you want to verify the API directly instead of the page:

Create a session:

```bash
curl -X POST http://127.0.0.1:3000/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

Send a turn:

```bash
curl -X POST http://127.0.0.1:3000/api/sessions/<SESSION_ID>/turn \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"Reply with exactly the single word ready. Do not call tools."}'
```

Read the latest state:

```bash
curl http://127.0.0.1:3000/api/sessions/<SESSION_ID>/state
```

If the transcript contains `ready`, the gateway, bridge, and `codex app-server` handshake are all working.

## Environment variables

- `HOST`: bind address for the Node server. Defaults to `0.0.0.0`.
- `PORT`: bind port. Defaults to `3000`.
- `CODEX_CWD`: working directory passed to `thread/start`. Defaults to the repository root.
- `CODEX_BIN`: path to the `codex` executable if it is not on `PATH`.
- `CODEX_MODEL`: preferred default model for new bridges.
- `MAX_SESSIONS`: maximum live sessions. Defaults to `12`.
- `SESSION_TTL_MS`: idle session TTL. Defaults to `1800000`.
- `SESSION_SWEEP_INTERVAL_MS`: cleanup sweep interval. Defaults to `60000`.
- `CODEX_HOME`: Codex runtime home for auth, logs, history, and config. In Docker this defaults to `/codex-home`.

## Docker

The container image installs the Codex CLI on Linux with `npm install -g @openai/codex`, which matches the official Codex CLI quickstart.

Build the image:

```bash
docker build -t codex-gateway-minimal .
```

Run it:

```bash
docker run --rm \
  -p 3000:3000 \
  -e HOST=0.0.0.0 \
  -e PORT=3000 \
  -e MAX_SESSIONS=8 \
  -v "$HOME/.codex:/codex-home" \
  codex-gateway-minimal
```

Notes:

- the mounted `CODEX_HOME` gives the container access to existing Codex auth/config state
- if you want Codex to operate on another workspace inside the container, set `CODEX_CWD` and mount that path too
- this is a PoC deployment shape, not a hardened public service
- after the container starts, use the same health/API/Web UI verification flow described above

## Current limitations

- no authentication or rate limiting
- no durable session persistence
- approval UI is intentionally absent
- each live session consumes a `codex app-server` subprocess
- browser clients reconnect with SSE, but session ownership is not persisted across page reloads unless the caller stores the session id
