# Codex Gateway

Chinese version: [README_zh.md](./README_zh.md)

HTTP/SSE gateway for Codex. Each session starts one `codex app-server` child process over stdio. The gateway stores that process's events in session state and streams them to the client over SSE. The same process also serves a small browser UI.

Sessions are separate processes. They share the gateway working directory. The gateway starts app-server with `sandbox_mode=danger-full-access` and `approval_policy=never`.

## Run locally

You need Rust 1.94.1 (`rust-toolchain.toml`) and `codex` on `PATH`.

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
cargo run --bin codex-gateway
```

Open [http://127.0.0.1:1317](http://127.0.0.1:1317).

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

Auth is off unless you set `CODEX_GATEWAY_JWT_SECRET`. Then every route except `/healthz` and `/readyz` needs `Authorization: Bearer <jwt>`. The JWT must be HS256 and include `exp`. EventSource cannot set headers, so SSE uses `?access_token=<jwt>`.

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <jwt>' \
  -d '{}'
```

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
| `POST` | `/api/sessions/:id/thread/new` | Start a new thread |
| `POST` | `/api/sessions/:id/thread/resume` | Resume a thread |
| `DELETE` | `/api/sessions/:id` | Close the session |
| `GET` | `/api/threads` | List threads |
| `GET` | `/api/threads/:threadId` | Read one thread |
| `POST` | `/api/brain/deployments` | Start a Brain deployment task |
| `GET` | `/api/brain/deployments/:threadId` | Poll deployment status |

`/api/state`, `/api/events`, `/api/turn`, and `/api/thread/new` return `410 Gone`.

### Sessions

`POST /api/sessions`

```json
{
  "model": "gpt-5",
  "resumeThreadId": "thread-1"
}
```

Both fields are optional. Empty body is valid.

```json
{
  "ok": true,
  "sessionId": "...",
  "session": {},
  "state": {}
}
```

`POST /api/sessions/:id/turn` body: `{ "prompt": "..." }`.  
`POST /api/sessions/:id/thread/new` body: `{ "model": "..." }` (`model` optional).  
`POST /api/sessions/:id/thread/resume` body: `{ "threadId": "..." }`.

`GET /api/threads` query: `cursor`, `limit`, `sortKey`, `archived`, `cwd`, `searchTerm`.

### Brain deployments

This is a Brain app endpoint. It is not a general deploy API.

It starts a Codex task in this gateway process. The task installs the deploy skill if needed, builds the repo image, pushes it to GHCR, and returns the image reference plus Sealos template YAML. There is no event stream and no follow-up user turn. Poll `GET` until `succeeded` or `failed`.

At most 4 deployments can run at once. Each task times out after 1 hour. Those limits are not env settings.

`POST /api/brain/deployments`

```json
{
  "githubToken": "ghp_...",
  "repository": "owner/repo",
  "branch": "main"
}
```

`githubToken` and `repository` are required. `repository` must be `owner/repo`. `branch` is optional.

`202 Accepted`:

```json
{
  "threadId": "...",
  "status": "running"
}
```

`GET /api/brain/deployments/:threadId`

```json
{
  "threadId": "...",
  "status": "running",
  "message": "...",
  "image": null,
  "template": null,
  "error": null
}
```

`status` is `running`, `succeeded`, or `failed`. `succeeded` requires a `ghcr.io/...` image and Sealos template content. On `running` or `failed`, `image` and `template` are `null`.

If the work must run inside a Devbox, create the Devbox and start this gateway there first, then call this API.

## Configuration

Gateway settings use the `CODEX_GATEWAY_` prefix.

| Variable | Meaning | Default |
| --- | --- | --- |
| `CODEX_GATEWAY_HOST` | Bind address | `0.0.0.0` |
| `CODEX_GATEWAY_PORT` | Bind port | `1317` |
| `CODEX_GATEWAY_CWD` | Working directory for `thread/start` | process cwd |
| `CODEX_GATEWAY_CODEX_BIN` | `codex` executable | `codex` |
| `CODEX_GATEWAY_CODEX_HOME` | Passed to the child as `CODEX_HOME` | unset |
| `CODEX_GATEWAY_MODEL` | Default model | unset |
| `CODEX_GATEWAY_DEBUG` | Set to `1` to log raw app-server lines | off |
| `CODEX_GATEWAY_OPENAI_API_KEY` | Used at startup for `codex login --with-api-key` | unset |
| `CODEX_GATEWAY_OPENAI_BASE_URL` | Upstream OpenAI-compatible base URL | unset |
| `CODEX_GATEWAY_JWT_SECRET` | HS256 secret; unset means no auth | unset |
| `CODEX_GATEWAY_MAX_SESSIONS` | Max live sessions | `12` |
| `CODEX_GATEWAY_SESSION_TTL_MS` | Idle session TTL | `1800000` |
| `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS` | Session cleanup interval | `60000` |

## Checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```
