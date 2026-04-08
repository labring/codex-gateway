# Codex Gateway

English version: [README.md](./README.md)

这个仓库当前的定位，是一个最小化的多 session `Codex gateway`，用于验证 `codex app-server` 能不能通过 Node HTTP/SSE 服务对外暴露。

## 当前形态

整体链路是：

1. 外部客户端调用 Node HTTP API
2. Node 服务为每个 session 创建一个 Codex bridge
3. 每个 bridge 启动一个自己的本地 `codex app-server` 子进程
4. `codex app-server` 的通知通过 SSE 回推给对应 session 的客户端

官方参考：

- [Codex App Server](https://developers.openai.com/codex/app-server/)
- [Codex CLI Quickstart](https://developers.openai.com/codex/quickstart/#setup)
- [Codex configuration reference](https://developers.openai.com/codex/config-reference/)
- [Codex CI/CD auth](https://developers.openai.com/codex/auth/ci-cd-auth)

## 目录说明

- `src/codex-app-server.mjs`：协议桥接层，负责 `initialize`、`account/read`、`model/list`、`thread/start`、`turn/start` 和通知处理
- `src/codex-runtime.mjs`：运行时辅助模块，负责 API key 登录和 `openai_base_url` 覆盖
- `src/bootstrap.mjs`：统一启动入口，会在启动 gateway 前按需执行 API key 登录
- `src/session-manager.mjs`：多 session 生命周期管理，包含 TTL 回收和 session 级事件转发
- `src/server.mjs`：HTTP 服务本体，提供 session API、SSE、健康检查和 Web UI
- `src/cli.mjs`：单次 CLI 冒烟验证
- `public/index.html`：最小化 Web UI
- `public/app.js`：浏览器端逻辑，会自动创建自己的 session 并订阅自己的 SSE
- `public/styles.css`：样式
- `Dockerfile`：Linux 容器运行镜像

## 运行模型

现在已经不是单个全局共享会话了。

- `POST /api/sessions` 会创建一个新 session
- 每个 session 拥有一个自己的 `CodexAppServerBridge`
- 每个 bridge 拥有一个自己的 `codex app-server` 子进程
- `/state`、`/events`、`/turn`、`/thread/new` 都是 session 级接口
- session 会在空闲超时后自动清理，也可以手动 `DELETE /api/sessions/:id`

这意味着多个调用方不会再共用同一个 thread 或 transcript。

## HTTP API

### 健康检查

- `GET /healthz`
- `GET /readyz`

### Session 接口

- `POST /api/sessions`
  - 请求体：`{ "model": "可选模型 ID" }`
  - 返回：`{ ok, sessionId, session, state }`
- `GET /api/sessions/:id/state`
  - 返回 session 信息和当前 bridge 状态快照
- `GET /api/sessions/:id/events`
  - 只属于该 session 的 SSE 流
- `POST /api/sessions/:id/turn`
  - 请求体：`{ "prompt": "..." }`
- `POST /api/sessions/:id/thread/new`
  - 请求体：`{ "model": "可选模型 ID" }`
- `DELETE /api/sessions/:id`
  - 关闭该 session 及其子进程

### 当前 PoC 行为

- 审批请求仍然会被自动拒绝
- 不支持的 server 发起请求会被拒绝
- session 状态只存在内存里，不持久化
- 当前没有 gateway 自己的鉴权
- 单个 session 同一时间只能有一个 active turn

## 本地运行

### Web UI

启动服务：

```bash
OPENAI_API_KEY=sk-... \
CODEX_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
npm start
```

打开：

```text
http://127.0.0.1:3000
```

页面会自动创建一个新的 session，自动连接它自己的 SSE 流，并在标签页关闭时尽量删除这个 session。

### CLI 冒烟

```bash
npm run cli
```

或自定义 prompt：

```bash
npm run cli -- "Reply with exactly the single word ready."
```

## 手动验证

如果你要快速验证这个项目能不能跑，建议按这个顺序：

1. 执行 `npm start`
2. 访问 `http://127.0.0.1:3000/healthz`
3. 访问 `http://127.0.0.1:3000/readyz`
4. 打开 `http://127.0.0.1:3000`
5. 等页面里的 `Status` 变成 `ready`
6. 发送 `Reply with exactly the single word ready. Do not call tools.`
7. 确认 Transcript 里出现 `ready`

如果你想直接验证 API，而不是页面：

创建 session：

```bash
curl -X POST http://127.0.0.1:3000/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

发送 turn：

```bash
curl -X POST http://127.0.0.1:3000/api/sessions/<SESSION_ID>/turn \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"Reply with exactly the single word ready. Do not call tools."}'
```

查看状态：

```bash
curl http://127.0.0.1:3000/api/sessions/<SESSION_ID>/state
```

如果 transcript 里出现 `ready`，就说明 gateway、bridge 和 `codex app-server` 之间的链路已经跑通。

## 环境变量

- `HOST`：Node 服务监听地址，默认 `0.0.0.0`
- `PORT`：监听端口，默认 `3000`
- `CODEX_CWD`：传给 `thread/start` 的工作目录，默认仓库根目录
- `CODEX_BIN`：`codex` 可执行文件路径，默认从 `PATH` 查找
- `CODEX_MODEL`：新 bridge 默认模型
- `OPENAI_API_KEY`：启动时用于执行 `codex login --with-api-key` 的 API key
- `CODEX_OPENAI_BASE_URL`：推荐使用的上游 OpenAI-compatible `base_url`，会映射成 Codex 的 `openai_base_url`
- `OPENAI_BASE_URL`：`CODEX_OPENAI_BASE_URL` 的兼容别名
- `MAX_SESSIONS`：最大同时在线 session 数，默认 `12`
- `SESSION_TTL_MS`：空闲 session TTL，默认 `1800000`
- `SESSION_SWEEP_INTERVAL_MS`：清理扫描间隔，默认 `60000`
- `CODEX_HOME`：Codex 运行目录，包含认证缓存、日志、历史和配置；Docker 默认值是 `/codex-home`

## Docker

容器镜像会通过 `npm install -g @openai/codex` 在 Linux 中安装 Codex CLI，这和官方 Quickstart 一致。

构建镜像：

```bash
docker build -t codex-gateway .
```

运行容器：

```bash
docker run --rm \
  -p 3000:3000 \
  -e OPENAI_API_KEY=sk-... \
  -e CODEX_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
  -e HOST=0.0.0.0 \
  -e PORT=3000 \
  -e MAX_SESSIONS=8 \
  codex-gateway
```

说明：

- 如果设置了 `OPENAI_API_KEY`，容器会在启动 gateway 前自动执行 `codex login --with-api-key`
- `CODEX_OPENAI_BASE_URL` 是把 Codex 指向第三方 OpenAI-compatible endpoint 的推荐方式
- 普通 API key 启动不需要挂载 `CODEX_HOME`；只有在你希望容器重启后保留 Codex 状态时才需要挂载
- 如果要让 Codex 在容器里操作别的工作目录，需要同时设置 `CODEX_CWD` 并挂载对应路径
- 这是 PoC 部署方式，不是生产加固版本
- 容器启动后，验证方法和本地运行时完全一样

## GitHub Container Registry

GitHub Actions 可以在推送到 `main` 以及版本 tag（例如 `v0.2.0`）后，把镜像发布到 GHCR。

发布出来的 tag 规则：

- `ghcr.io/che-zhu/codex-gateway:main` 表示当前 `main` 分支最新镜像
- `ghcr.io/che-zhu/codex-gateway:sha-<commit>` 表示每次发布对应的提交镜像
- 推送版本 tag 时，会额外发布 `v0.2.0`、`0.2.0`、`0.2`、`0` 和 `latest`

拉取当前 `main` 镜像：

```bash
docker pull ghcr.io/che-zhu/codex-gateway:main
```

运行方式和本地构建镜像一致：

```bash
docker run --rm \
  -p 3000:3000 \
  -e OPENAI_API_KEY=sk-... \
  -e CODEX_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
  -e HOST=0.0.0.0 \
  -e PORT=3000 \
  -e MAX_SESSIONS=8 \
  ghcr.io/che-zhu/codex-gateway:main
```

如果包可见性是私有的，拉取前需要先登录 GHCR。

## 当前限制

- 没有 gateway 自己的鉴权和限流
- 没有持久化 session
- 没有审批 UI
- 每个活跃 session 都会占用一个 `codex app-server` 子进程
- 浏览器刷新后不会自动恢复原 session，除非调用方自己保存 `sessionId`
