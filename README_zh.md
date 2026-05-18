# Codex Gateway

英文版：[README.md](./README.md)

Codex Gateway 是一个 Rust HTTP/SSE 网关，用来通过小型 API 和浏览器 UI 运行相互隔离的 Codex session。

`.qoder/` 下可能存在 Qoder 自动生成文档；它是生成物，不作为人工源码文档编辑或提交。这个 README 是当前仓库维护的人工入口。

## Runtime 形态

默认 runtime 是 `embedded`：

1. 客户端通过 Rust gateway 创建 session。
2. session 拥有一个 `CodexAppServerBridge`。
3. bridge 通过 stdio 启动并管理一个 `codex app-server` 子进程。
4. app-server 的通知会写入 session state，并通过 SSE 推给客户端。

可选的 `devbox` runtime 是远端执行后端：

1. 外层 gateway 创建 Devbox runtime。
2. 外层 gateway 等待 Devbox 内部的 gateway ready。
3. 外层 gateway 在内部 gateway 里创建远端 session。
4. 内部 gateway 使用 `embedded` runtime 运行 `codex app-server`。

`devbox` 是 runtime 基础设施，不是产品模式。

## Brain Deployment API

`POST /api/deployments` 是为 Brain 应用预留的接口，不是通用部署产品接口。

这个接口会创建一个 Codex task，用来部署仓库并返回机器可读的部署结果。当当前 session runtime 由 Devbox 承载时，Gateway 会先 bootstrap Devbox runtime，再启动 Brain deployment task。

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

旧的单 session 路由已经移除，例如 `/api/state`、`/api/events`、`/api/turn`、`/api/thread/new`，现在会返回 `410 Gone`。

## 本地运行

启动 gateway：

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
CODEX_GATEWAY_JWT_SECRET=replace-with-your-hs256-secret \
cargo run --bin codex-gateway
```

打开：

```text
http://127.0.0.1:1317
```

快速 API 验证：

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

## 配置

Gateway 自有配置统一使用 `CODEX_GATEWAY_` 前缀。

- `CODEX_GATEWAY_HOST`：监听地址，默认 `0.0.0.0`
- `CODEX_GATEWAY_PORT`：监听端口，默认 `1317`
- `CODEX_GATEWAY_CWD`：传给 `thread/start` 的工作目录
- `CODEX_GATEWAY_CODEX_BIN`：`codex` 可执行文件路径
- `CODEX_GATEWAY_MODEL`：默认模型
- `CODEX_GATEWAY_OPENAI_API_KEY`：启动时用于执行 `codex login --with-api-key` 的 API key
- `CODEX_GATEWAY_OPENAI_BASE_URL`：上游 OpenAI-compatible base URL
- `CODEX_GATEWAY_JWT_SECRET`：可选 HS256 JWT secret
- `CODEX_GATEWAY_MAX_SESSIONS`：最大在线 session 数，默认 `12`
- `CODEX_GATEWAY_SESSION_TTL_MS`：空闲 session TTL，默认 `1800000`
- `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS`：清理扫描间隔，默认 `60000`
- `CODEX_GATEWAY_SESSION_RUNTIME`：session runtime backend，默认 `embedded`；支持值只有 `embedded` 和 `devbox`
- `CODEX_GATEWAY_MAX_DEPLOYMENTS`：最大并发 Brain deployment task 数，默认 `4`
- `CODEX_GATEWAY_DEPLOYMENT_TIMEOUT_MS`：Brain deployment 超时和 session keepalive 窗口，默认 `3600000`

Devbox 相关配置只在 runtime 为 `devbox` 时使用：

- `CODEX_GATEWAY_DEVBOX_BASE_URL`
- `CODEX_GATEWAY_DEVBOX_TOKEN`
- `CODEX_GATEWAY_DEVBOX_JWT_SIGNING_KEY`
- `CODEX_GATEWAY_DEVBOX_NAMESPACE`
- `CODEX_GATEWAY_DEVBOX_RUNTIME_IMAGE`
- `CODEX_GATEWAY_DEVBOX_ARCHIVE_AFTER_PAUSE_TIME`
- `CODEX_GATEWAY_DEVBOX_WAIT_TIMEOUT_SECONDS`
- `CODEX_GATEWAY_DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS`
- `CODEX_GATEWAY_DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS`

## 验证

```bash
cargo fmt --check
cargo test
```
