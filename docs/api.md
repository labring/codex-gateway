# API Reference

本文档列出当前 Rust gateway 支持的 HTTP API 和 SSE 事件。

默认本地地址：

```text
http://127.0.0.1:1317
```

如果启动时设置了 `CODEX_GATEWAY_PORT`，端口以实际配置为准。

## Auth

默认不启用鉴权。

如果服务端设置了 `CODEX_GATEWAY_JWT_SECRET`，除以下两个接口外，其他路由都需要合法 JWT：

- `GET /healthz`
- `GET /readyz`

普通 HTTP 请求使用：

```http
Authorization: Bearer <JWT>
```

SSE 请求也支持 query 参数：

```text
/api/sessions/:id/events?access_token=<JWT>
```

或：

```text
/api/sessions/:id/events?token=<JWT>
```

JWT 当前使用 HS256 校验，并要求包含 `exp`。

## Common Response Objects

### Session

```json
{
  "id": "session-id",
  "createdAt": "2026-04-15T01:00:00Z",
  "lastAccessAt": "2026-04-15T01:00:00Z",
  "expiresAt": "2026-04-15T01:30:00Z"
}
```

### State

`state` 是当前 session 对应 bridge 的状态快照，主要字段包括：

```json
{
  "ready": true,
  "cwd": "/workspace",
  "startedAt": "2026-04-15T01:00:00Z",
  "runtime": {},
  "account": {},
  "models": [],
  "selectedModel": "gpt-5.4",
  "threadId": "thread-id",
  "threadStatus": { "type": "idle" },
  "currentTurnId": null,
  "activeTurn": false,
  "lastTurnStatus": null,
  "transcript": [],
  "recentEvents": []
}
```

## Endpoints

### GET /healthz

健康检查。这个接口不需要鉴权。

Response:

```json
{
  "ok": true,
  "uptimeSeconds": 12
}
```

### GET /readyz

就绪检查。这个接口不需要鉴权。

Response:

```json
{
  "ok": true,
  "activeSessions": 0
}
```

### POST /api/sessions

创建一个新的 gateway session。

每个 session 会启动一个独立的 `codex app-server` 子进程。

Request body 可以为空，也可以传模型：

```json
{
  "model": "gpt-5.4"
}
```

Response:

```json
{
  "ok": true,
  "sessionId": "session-id",
  "session": {
    "id": "session-id",
    "createdAt": "2026-04-15T01:00:00Z",
    "lastAccessAt": "2026-04-15T01:00:00Z",
    "expiresAt": "2026-04-15T01:30:00Z"
  },
  "state": {}
}
```

常见错误：

- `503`：达到最大并发 session 数。
- `500`：启动或初始化 `codex app-server` 失败。

### GET /api/sessions/:id/state

获取指定 session 的当前状态快照。

Response:

```json
{
  "ok": true,
  "sessionId": "session-id",
  "session": {},
  "state": {}
}
```

常见错误：

- `404`：session 不存在或已过期。

### GET /api/sessions/:id/events

订阅指定 session 的 SSE 事件流。

连接建立后，服务端会先发送两个事件：

```text
event: session
data: {...}

event: state
data: {...}
```

后续可能出现的事件：

| Event | 含义 |
| --- | --- |
| `session` | 当前 session metadata。连接建立时发送。 |
| `state` | 当前 bridge 状态快照。状态变化时发送。 |
| `notification` | `codex app-server` 发来的普通通知。 |
| `server-request` | `codex app-server` 发起的请求，例如 approval request。当前命令执行和文件修改 approval request 会被自动接受。 |
| `warning` | gateway 或 bridge 产生的警告。 |
| `raw` | 原始 app-server 消息。仅在 `CODEX_GATEWAY_DEBUG=1` 时可能出现。 |
| `session-closed` | session 被关闭、过期或 gateway shutdown。 |

连接保活：

- 服务端每 15 秒发送一次 SSE keepalive。

常见错误：

- `401`：开启鉴权后 token 缺失或无效。
- `404`：session 不存在或已过期。

### POST /api/sessions/:id/turn

向指定 session 的当前 thread 发送一次用户输入。

Request body:

```json
{
  "prompt": "Reply with exactly OK."
}
```

Response:

```json
{
  "ok": true,
  "sessionId": "session-id",
  "session": {},
  "state": {}
}
```

说明：

- 这个接口只负责启动一次 turn。
- 过程输出主要通过 `GET /api/sessions/:id/events` 获取。
- 同一个 session 同一时间只允许一个 active turn。
- 如果要停止当前回复，调用 `POST /api/sessions/:id/turn/interrupt`。

常见错误：

- `400`：`prompt` 为空或请求体不是合法 JSON。
- `404`：session 不存在或已过期。
- `409`：当前 session 已经有 active turn。

### POST /api/sessions/:id/turn/interrupt

请求停止指定 session 里正在运行的当前 turn。

Request body 可以为空。

Response:

```json
{
  "ok": true,
  "sessionId": "session-id",
  "session": {},
  "state": {}
}
```

说明：

- 这个接口对应 `codex app-server` 的 `turn/interrupt`。
- session 会保留。
- 当前 thread 会保留。
- 已有上下文会保留。
- 当前 turn 会结束为 `interrupted` 状态。
- 这适合实现产品里的 “Stop generating”。
- 这不是删除 session。

常见错误：

- `404`：session 不存在或已过期。
- `409`：当前没有 active turn，或 active turn 还没有可中断的 `turnId`。
- `500`：`turn/interrupt` 调用失败。

### POST /api/sessions/:id/thread/new

在同一个 session 内新开一个 thread。

Request body 可以为空，也可以传模型：

```json
{
  "model": "gpt-5.4"
}
```

Response:

```json
{
  "ok": true,
  "sessionId": "session-id",
  "session": {},
  "state": {}
}
```

说明：

- session 保留。
- `codex app-server` 子进程保留。
- 当前 transcript 会被清空。
- 新 thread 使用请求里的 `model`，没有传时复用当前 selected model。

常见错误：

- `404`：session 不存在或已过期。
- `500`：`thread/start` 失败。

### DELETE /api/sessions/:id

删除指定 session。

Response:

```json
{
  "ok": true,
  "sessionId": "session-id"
}
```

说明：

- 这是关闭整个 session 的接口。
- 会停止对应 Bridge。
- 会 kill 对应的 `codex app-server` 子进程。
- session 的内存状态会丢失。
- 如果只想停止当前 AI 回复并保留上下文，请用 `POST /api/sessions/:id/turn/interrupt`。

常见错误：

- `404`：session 不存在或已过期。

## Static Routes

这些路由服务内置 Web UI，也会经过可选 JWT 鉴权：

| Method | Path | 用途 |
| --- | --- | --- |
| `GET` | `/` | Web UI HTML |
| `GET` | `/app.js` | Web UI JavaScript |
| `GET` | `/styles.css` | Web UI CSS |

## Removed Legacy Routes

旧的单 session API 已移除。以下路由当前会返回 `410 Gone`：

| Method | Path |
| --- | --- |
| `GET`, `POST` | `/api/state` |
| `GET`, `POST` | `/api/events` |
| `GET`, `POST` | `/api/turn` |
| `GET`, `POST` | `/api/thread/new` |

Response:

```json
{
  "error": "Legacy single-session endpoints were removed. Create a session first via POST /api/sessions."
}
```

## Error Format

普通 JSON 错误响应格式：

```json
{
  "error": "message"
}
```

常见状态码：

| Status | 含义 |
| --- | --- |
| `400` | 请求体不是合法 JSON，或必要字段为空。 |
| `401` | 开启鉴权后 token 缺失或无效。 |
| `404` | 路由不存在，或 session 不存在。 |
| `409` | 当前 session 已经有 active turn，或没有可中断的 active turn。 |
| `410` | 访问已移除的旧单 session API。 |
| `503` | 达到最大并发 session 数。 |
| `500` | 子进程、Codex app-server 或内部状态错误。 |
