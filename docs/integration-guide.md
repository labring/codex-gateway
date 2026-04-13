# Codex Gateway 对接说明

## 1. 推荐对接流程

一个完整接入流程通常是：

1. 调用 `POST /api/sessions` 创建 session
2. 保存返回的 `sessionId`
3. 连接 `GET /api/sessions/:id/events` 订阅该 session 的 SSE
4. 调用 `POST /api/sessions/:id/turn` 发送用户输入
5. 通过 SSE 或 `GET /api/sessions/:id/state` 获取当前状态和结果
6. 使用结束后调用 `DELETE /api/sessions/:id`

推荐把一个页面、一个用户会话、或一个业务任务映射到一个 gateway session。

## 2. 接口说明

### 2.1 创建 session

**请求**

```http
POST /api/sessions
Content-Type: application/json
```

可选请求体：

```json
{
  "model": "gpt-5.4"
}
```

**响应示例**

```json
{
  "ok": true,
  "sessionId": "9d7a5c2d-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "session": {
    "id": "9d7a5c2d-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
    "createdAt": "2026-04-09T02:00:00.000Z",
    "lastAccessAt": "2026-04-09T02:00:00.000Z",
    "expiresAt": "2026-04-09T02:30:00.000Z"
  },
  "state": {
    "ready": true,
    "selectedModel": "gpt-5.4"
  }
}
```

说明：

- `sessionId` 是后续所有操作的主键
- 一个 session 对应一个独立的 `codex app-server` 子进程
- 如果不传 `model`，会使用服务端默认模型

### 2.2 订阅事件流

**请求**

```http
GET /api/sessions/:id/events
Accept: text/event-stream
```

这是一个 SSE 长连接，用来接收该 session 的状态变化和消息事件。

建议：

- 创建 session 后尽快连接 SSE
- 前端做自动重连
- 业务侧自己保存 `sessionId`，不要依赖页面刷新后自动恢复

### 2.3 发送 prompt

**请求**

```http
POST /api/sessions/:id/turn
Content-Type: application/json
```

请求体：

```json
{
  "prompt": "请帮我总结这个目录下的代码结构"
}
```

**响应示例**

```json
{
  "ok": true,
  "sessionId": "9d7a5c2d-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "session": {
    "id": "9d7a5c2d-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
    "createdAt": "2026-04-09T02:00:00.000Z",
    "lastAccessAt": "2026-04-09T02:00:10.000Z",
    "expiresAt": "2026-04-09T02:30:10.000Z"
  },
  "state": {
    "ready": true,
    "activeTurn": true
  }
}
```

说明：

- 这个接口只负责启动一次 turn
- 过程性输出主要通过 SSE 观察
- 最终结果也可以通过 `GET /api/sessions/:id/state` 中的 `transcript` 查看

### 2.4 查询状态

**请求**

```http
GET /api/sessions/:id/state
```

这个接口返回当前 session 的完整状态快照，适合以下场景：

- 页面初始化时补一次状态
- SSE 中断后做兜底同步
- 排查问题时查看 `transcript`、`selectedModel`、`threadStatus`

### 2.5 新开 thread

**请求**

```http
POST /api/sessions/:id/thread/new
Content-Type: application/json
```

可选请求体：

```json
{
  "model": "gpt-5.4"
}
```

用途：

- 在保留同一个 session 的前提下，新开一个 thread
- 适合需要“清空上下文重新开始”，但又不想重建整个 session 的场景

### 2.6 删除 session

**请求**

```http
DELETE /api/sessions/:id
```

用途：

- 主动释放这个 session 对应的 `codex app-server` 子进程
- 页面退出、任务完成、或会话明确结束时，建议主动调用

## 2.7 可选鉴权

如果服务端设置了 `CODEX_GATEWAY_JWT_SECRET`，除了 `/healthz` 和 `/readyz` 以外，其他路由都需要携带合法的 HS256 JWT。

普通 HTTP 请求请带：

```http
Authorization: Bearer <JWT>
```

SSE 场景如果不方便设置请求头，也可以通过 query 参数传：

```text
/api/sessions/:id/events?access_token=<JWT>
```

## 3. 最小示例

### 3.1 创建 session

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Authorization: Bearer <JWT>' \
  -H 'Content-Type: application/json' \
  -d '{}'
```

### 3.2 发起一次 turn

```bash
curl -X POST http://127.0.0.1:1317/api/sessions/<SESSION_ID>/turn \
  -H 'Authorization: Bearer <JWT>' \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"Reply with exactly OK."}'
```

### 3.3 查询状态

```bash
curl http://127.0.0.1:1317/api/sessions/<SESSION_ID>/state \
  -H 'Authorization: Bearer <JWT>'
```

### 3.4 删除 session

```bash
curl -X DELETE http://127.0.0.1:1317/api/sessions/<SESSION_ID> \
  -H 'Authorization: Bearer <JWT>'
```

## 4. 前端接入示例

下面是一个最小的浏览器侧接入思路：

```js
const token = "<JWT>";

const createResponse = await fetch("/api/sessions", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  },
  body: JSON.stringify({ model: "gpt-5.4" }),
});

const created = await createResponse.json();
const sessionId = created.sessionId;

const events = new EventSource(`/api/sessions/${sessionId}/events`);
events.onmessage = (event) => {
  console.log("sse message", event.data);
};

await fetch(`/api/sessions/${sessionId}/turn`, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ prompt: "请解释一下这个仓库的用途" }),
});
```

如果要在标签页关闭时清理 session，可以在页面退出前补一个删除调用。

## 5. Session 生命周期

一个 session 会在以下情况下被销毁：

- 调用 `DELETE /api/sessions/:id` 显式删除
- 超过空闲 TTL 被自动回收
- gateway 进程退出时统一清理

当前默认值是：

- `CODEX_GATEWAY_SESSION_TTL_MS = 1800000`
- `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS = 60000`

也就是：

- 默认空闲 30 分钟自动过期
- 每 60 秒扫描一次过期 session

只要该 session 还有 API 调用或内部事件流动，它的 `expiresAt` 就会刷新。

## 6. 资源占用和边界

接入方需要特别注意以下几点：

- 每个活跃 session 都会占用一个独立的 `codex app-server` 子进程
- 一个 gateway 实例当前默认只使用一套统一的 API key
- 当前不支持“每个 session 使用不同 API key”
- 当前没有内建用户鉴权、配额、限流和持久化 ownership

所以建议：

- 一个页面或一个任务尽量复用一个 session
- 用完及时删除，不要无限创建
- 如果要对公网开放，前面最好再加一层业务鉴权或接入层

## 7. 部署侧需要准备什么

对接方通常不需要关心服务如何部署，但部署方至少需要配置：

- `CODEX_GATEWAY_OPENAI_API_KEY`
- `CODEX_GATEWAY_OPENAI_BASE_URL`

当前 gateway 会把 `CODEX_GATEWAY_OPENAI_BASE_URL` 映射成一个自定义 Codex provider，并显式关闭 websocket transport，以兼容当前使用的第三方 OpenAI-compatible upstream。
