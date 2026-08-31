# Codex Gateway

英文版：[README.md](./README.md)

Codex 的 HTTP/SSE 网关。每个 session 通过 stdio 拉起一个 `codex app-server` 子进程。网关把该进程的事件写入 session 状态，再用 SSE 推给客户端。同一进程也提供一个浏览器 UI。

session 是彼此独立的进程，但共用网关的工作目录。网关启动 app-server 时使用 `sandbox_mode=danger-full-access` 和 `approval_policy=never`。

## 本地运行

需要 Rust 1.94.1（见 `rust-toolchain.toml`），并且 `PATH` 上有 `codex`。

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
cargo run --bin codex-gateway
```

打开 [http://127.0.0.1:1317](http://127.0.0.1:1317)。

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

默认不鉴权。设置 `CODEX_GATEWAY_JWT_SECRET` 之后，除 `/healthz` 和 `/readyz` 外都要带 `Authorization: Bearer <jwt>`。JWT 必须是 HS256，且包含 `exp`。EventSource 不能设请求头，SSE 用 `?access_token=<jwt>`。

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <jwt>' \
  -d '{}'
```

## Docker

镜像是 `linux/amd64`，地址 `ghcr.io/labring/codex-gateway`。

```bash
docker run --rm -p 1317:1317 \
  -e CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
  -e CODEX_GATEWAY_OPENAI_BASE_URL=https://example-openai-compatible-endpoint.test \
  ghcr.io/labring/codex-gateway:main
```

镜像里 `CODEX_GATEWAY_MAX_SESSIONS=8`。不设该变量时，进程默认是 `12`。

## HTTP API

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| `GET` | `/healthz` | 存活检查 |
| `GET` | `/readyz` | 进程已起来；含 `activeSessions` |
| `POST` | `/api/sessions` | 创建 session |
| `GET` | `/api/sessions/:id/state` | session 快照 |
| `GET` | `/api/sessions/:id/events` | SSE 事件流 |
| `POST` | `/api/sessions/:id/turn` | 发送 prompt |
| `POST` | `/api/sessions/:id/turn/interrupt` | 中断当前 turn |
| `POST` | `/api/sessions/:id/thread/new` | 开新 thread |
| `POST` | `/api/sessions/:id/thread/resume` | 恢复 thread |
| `DELETE` | `/api/sessions/:id` | 关闭 session |
| `GET` | `/api/threads` | 列出 thread |
| `GET` | `/api/threads/:threadId` | 读取一个 thread |
| `POST` | `/api/brain/deployments` | 启动 Brain 部署任务 |
| `GET` | `/api/brain/deployments/:threadId` | 轮询部署状态 |

`/api/state`、`/api/events`、`/api/turn`、`/api/thread/new` 返回 `410 Gone`。

### Session

`POST /api/sessions`

```json
{
  "model": "gpt-5",
  "resumeThreadId": "thread-1"
}
```

两个字段都可省略。空 body 合法。

```json
{
  "ok": true,
  "sessionId": "...",
  "session": {},
  "state": {}
}
```

`POST /api/sessions/:id/turn` 的 body：`{ "prompt": "..." }`。  
`POST /api/sessions/:id/thread/new` 的 body：`{ "model": "..." }`（`model` 可省略）。  
`POST /api/sessions/:id/thread/resume` 的 body：`{ "threadId": "..." }`。

`GET /api/threads` 的 query：`cursor`、`limit`、`sortKey`、`archived`、`cwd`、`searchTerm`。

### Brain 部署

这是 Brain 应用接口，不是通用部署接口。

它在本网关进程里起一个 Codex 任务：需要时安装部署 skill，构建仓库镜像，推到 GHCR，返回镜像地址和 Sealos template YAML。没有事件流，也不能再发用户 turn。用 `GET` 轮询，直到 `succeeded` 或 `failed`。

同时最多 4 个部署任务。每个任务 1 小时超时。这两个限制不能用环境变量改。

`POST /api/brain/deployments`

```json
{
  "githubToken": "ghp_...",
  "repository": "owner/repo",
  "branch": "main"
}
```

`githubToken` 和 `repository` 必填。`repository` 必须是 `owner/repo`。`branch` 可省略。

`202 Accepted`：

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

`status` 为 `running`、`succeeded` 或 `failed`。`succeeded` 要求结果里有 `ghcr.io/...` 镜像和 Sealos template 内容。`running` 或 `failed` 时，`image` 和 `template` 为 `null`。

如果部署必须跑在 Devbox 里，先由外部系统创建 Devbox 并在其中启动这个网关，再调这个接口。

## 配置

网关配置一律使用 `CODEX_GATEWAY_` 前缀。

| 变量 | 含义 | 默认 |
| --- | --- | --- |
| `CODEX_GATEWAY_HOST` | 监听地址 | `0.0.0.0` |
| `CODEX_GATEWAY_PORT` | 监听端口 | `1317` |
| `CODEX_GATEWAY_CWD` | 传给 `thread/start` 的工作目录 | 进程 cwd |
| `CODEX_GATEWAY_CODEX_BIN` | `codex` 可执行文件 | `codex` |
| `CODEX_GATEWAY_CODEX_HOME` | 传给子进程的 `CODEX_HOME` | 未设置 |
| `CODEX_GATEWAY_MODEL` | 默认模型 | 未设置 |
| `CODEX_GATEWAY_DEBUG` | 设为 `1` 时打印 app-server 原始行 | 关闭 |
| `CODEX_GATEWAY_OPENAI_API_KEY` | 启动时用于 `codex login --with-api-key` | 未设置 |
| `CODEX_GATEWAY_OPENAI_BASE_URL` | 上游 OpenAI-compatible 地址 | 未设置 |
| `CODEX_GATEWAY_JWT_SECRET` | HS256 密钥；不设则不鉴权 | 未设置 |
| `CODEX_GATEWAY_MAX_SESSIONS` | 最大在线 session 数 | `12` |
| `CODEX_GATEWAY_SESSION_TTL_MS` | 空闲 session TTL | `1800000` |
| `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS` | session 清理间隔 | `60000` |

## 检查

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```
