# Codex Gateway

英文版：[README.md](./README.md)

Codex 的 HTTP/SSE 网关。每个 session 通过 stdio 拉起一个 `codex app-server` 子进程；网关驱动 turn 执行，把事件用 SSE 推给客户端，并可选地把用户与 Agent 的每次交互记录到 [Langfuse](https://langfuse.com/)。

session 是彼此独立的进程，共用网关的工作目录。网关启动 app-server 时使用 `sandbox_mode=danger-full-access` 和 `approval_policy=never`，并自动应答审批请求：它就是为一次性沙箱（Sealos Devbox）设计的。

## 本地运行

需要 Rust 1.94.1（见 `rust-toolchain.toml`），并且 `PATH` 上有 `codex`。

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

默认不鉴权。设置 `CODEX_GATEWAY_JWT_SECRET` 之后，除 `/healthz` 和 `/readyz` 外都要带 `Authorization: Bearer <jwt>`。JWT 必须是 HS256，且包含 `exp`。EventSource 不能设请求头，SSE 支持 `?access_token=<jwt>`。

`codex-gateway-cli` 是冒烟工具：对一个新的 app-server 跑一条 prompt 并打印最终回复。

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
| `DELETE` | `/api/sessions/:id` | 关闭 session |

### Session

`POST /api/sessions`

```json
{
  "model": "gpt-5",
  "resumeThreadId": "thread-1"
}
```

两个字段都可选；空 body 表示用默认模型开新 thread。`threadId` 是 `resumeThreadId` 的别名。

```json
{
  "ok": true,
  "sessionId": "...",
  "session": {},
  "state": {}
}
```

`state` 为 `{ ready, cwd, startedAt, selectedModel, threadId, currentTurnId, activeTurn, lastTurnStatus }`。网关不再镜像 thread 历史；需要历史时从 Codex 或 Langfuse 读取。

`POST /api/sessions/:id/turn` body：`{ "prompt": "..." }`。turn 进行中会返回 `409`。

### SSE 事件

`GET /api/sessions/:id/events` 连接时先发 `session` 和 `state`，之后是 `state`、`notification`（app-server 原始 JSON-RPC 通知）、`server-request`、`warning`、`session-closed`；设置 `CODEX_GATEWAY_DEBUG=1` 时还有 `raw`。

## Langfuse 追踪

设置 `LANGFUSE_PUBLIC_KEY` 和 `LANGFUSE_SECRET_KEY` 即可记录用户与 Agent 的全部交互。span 通过 OTLP（HTTP/protobuf）发到 `{LANGFUSE_HOST}/api/public/otel/v1/traces`，后台线程批量导出；Langfuse 故障最多丢遥测，绝不阻塞请求。

每个 turn 是一条 trace：root span 的 input 是 prompt，output 是最终 Agent 回复，并带 token 用量。每个完成的 codex item 是一个子 observation（命令执行及其输出、文件修改、MCP 工具调用、Agent 回复作为 generation）。app-server 错误、网关告警、自动应答的审批请求记为 event。已知凭证形态（GitHub token、API key、Bearer token）在导出前脱敏，payload 截断到 8 KiB。

trace 身份来自 Brain 创建 Devbox 时注入的环境变量：`langfuse.user.id` = `SEALAI_NAMESPACE`，`langfuse.session.id` = `SEALAI_DEPLOY_TASK_ID`（缺省回退 codex thread id），`SEALAI_PROJECT_ID` 进 trace metadata——排查失败的部署任务时，拿 task id 到 Langfuse 直接搜索。

## 配置

| 变量 | 含义 | 默认值 |
| --- | --- | --- |
| `CODEX_GATEWAY_HOST` | 监听地址 | `0.0.0.0` |
| `CODEX_GATEWAY_PORT` | 监听端口 | `1317` |
| `CODEX_GATEWAY_CWD` | `thread/start` 的工作目录 | 进程 cwd |
| `CODEX_GATEWAY_CODEX_BIN` | `codex` 可执行文件 | `codex` |
| `CODEX_GATEWAY_CODEX_HOME` | 以 `CODEX_HOME` 传给子进程 | 未设置 |
| `CODEX_GATEWAY_MODEL` | 默认模型 | 未设置 |
| `CODEX_GATEWAY_DEBUG` | 设为 `1` 记录并转发 app-server 原始行 | 关 |
| `CODEX_GATEWAY_OPENAI_API_KEY` | 启动时用于 `codex login --with-api-key` | 未设置 |
| `CODEX_GATEWAY_OPENAI_BASE_URL` | 上游 OpenAI 兼容端点 | 未设置 |
| `CODEX_GATEWAY_JWT_SECRET` | HS256 密钥；未设置则不鉴权 | 未设置 |
| `CODEX_GATEWAY_MAX_SESSIONS` | 最大并发 session 数 | `12` |
| `CODEX_GATEWAY_SESSION_TTL_MS` | 空闲 session TTL | `1800000` |
| `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS` | session 清理间隔 | `60000` |
| `LANGFUSE_PUBLIC_KEY` | Langfuse 项目 public key；未设置则不追踪 | 未设置 |
| `LANGFUSE_SECRET_KEY` | Langfuse 项目 secret key | 未设置 |
| `LANGFUSE_HOST` | Langfuse 地址 | `https://cloud.langfuse.com` |
| `SEALAI_NAMESPACE` | Langfuse `user.id` | 未设置 |
| `SEALAI_DEPLOY_TASK_ID` | Langfuse `session.id` | codex thread id |
| `SEALAI_PROJECT_ID` | trace metadata 里的 `projectId` | 未设置 |

## 检查

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

集成测试会用真实网关二进制对接假的 `codex app-server` 和假的 Langfuse OTLP 端点。
