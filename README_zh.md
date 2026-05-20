# Codex Gateway

英文版：[README.md](./README.md)

Codex Gateway 是一个 Rust HTTP/SSE 网关，用来通过小型 API 和浏览器 UI 运行相互隔离的 Codex session。

`.qoder/` 下可能存在 Qoder 自动生成文档；它是生成物，不作为人工源码文档编辑或提交。这个 README 是当前仓库维护的人工入口。

## Runtime 形态

所有 Gateway session 都使用 embedded runtime：

1. 客户端通过 Rust gateway 创建 session。
2. session 拥有一个 `CodexAppServerBridge`。
3. bridge 通过 stdio 启动并管理一个 `codex app-server` 子进程。
4. app-server 的通知会写入 session state，并通过 SSE 推给客户端。

Brain deployment task 也使用同一个 embedded runtime，但对外暴露的是 polling-only 的任务 API，而不是可交互 session API。Gateway 收到部署请求时不会创建或管理 Devbox runtime。

如果部署工作运行在 Devbox 中，外部系统负责先创建 Devbox，并在 Devbox 内启动这个 gateway，然后再调用 Brain Deployment API。

## Brain Deployment API

`POST /api/brain/deployments` 是 Brain 应用接口，不是通用部署产品接口。

这个接口会创建一个本地 embedded Codex task。该 task 会按需安装 deployment skill，构建仓库镜像，推送到 GHCR，生成 Sealos template，并返回同时包含镜像地址和 template 内容的机器可读部署结果。接口不暴露 Codex 中间输出，也不接受用户继续输入。

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

Devbox 生命周期在 gateway 外部管理。如果 gateway 运行在 Devbox 中，仍然只需要配置上面的常规 gateway 设置。

## 验证

```bash
cargo fmt --check
cargo test
```
