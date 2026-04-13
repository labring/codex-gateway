# Codex Gateway

English version: [README.md](./README.md)

这个仓库当前的定位，是一个最小化的多 session `Codex gateway`，用于验证 `codex app-server` 能不能通过 Rust HTTP/SSE 服务对外暴露。

## 当前形态

整体链路是：

1. 外部客户端调用 Rust HTTP API
2. Rust 服务为每个 session 创建一个 Codex bridge
3. 每个 bridge 启动一个自己的本地 `codex app-server` 子进程
4. `codex app-server` 的通知通过 SSE 回推给对应 session 的客户端

官方参考：

- [Codex App Server](https://developers.openai.com/codex/app-server/)
- [Codex CLI Quickstart](https://developers.openai.com/codex/quickstart/#setup)
- [Codex configuration reference](https://developers.openai.com/codex/config-reference/)
- [Codex CI/CD auth](https://developers.openai.com/codex/auth/ci-cd-auth)

## 目录说明

- `rust-src/main.rs`：Rust HTTP 服务本体，提供 session API、SSE、健康检查和静态文件
- `rust-src/bridge.rs`：协议桥接层，负责 `initialize`、`account/read`、`model/list`、`thread/start`、`turn/start` 和通知处理
- `rust-src/runtime.rs`：运行时辅助模块，负责 API key 登录和 `openai_base_url` 覆盖
- `rust-src/session_manager.rs`：多 session 生命周期管理，包含 TTL 回收
- `rust-src/cli.rs`：单次 CLI 冒烟验证
- `src/*.mjs`：上一版 Node 实现，暂时保留作为迁移参考
- `public/index.html`：最小化 Web UI
- `public/app.js`：浏览器端逻辑，会自动创建自己的 session 并订阅自己的 SSE
- `public/styles.css`：样式
- `Dockerfile`：多阶段容器镜像，负责构建 Rust 二进制并安装 Codex CLI

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
- gateway 鉴权是可选的，只有设置 `CODEX_GATEWAY_JWT_SECRET` 时才会开启
- 单个 session 同一时间只能有一个 active turn

## 本地运行

### Web UI

启动服务：

```bash
CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
CODEX_GATEWAY_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
CODEX_GATEWAY_JWT_SECRET=replace-with-your-hs256-secret \
cargo run --bin codex-gateway
```

打开：

```text
http://127.0.0.1:1317
```

页面会自动创建一个新的 session，自动连接它自己的 SSE 流，并在标签页关闭时尽量删除这个 session。开启 JWT 鉴权后，需要先在侧边栏的 `Auth` 输入框里填入 Bearer token。

### CLI 冒烟

```bash
cargo run --bin codex-gateway-cli --
```

或自定义 prompt：

```bash
cargo run --bin codex-gateway-cli -- "Reply with exactly the single word ready."
```

## 手动验证

如果你要快速验证这个项目能不能跑，建议按这个顺序：

1. 执行 `cargo run --bin codex-gateway`
2. 访问 `http://127.0.0.1:1317/healthz`
3. 访问 `http://127.0.0.1:1317/readyz`
4. 打开 `http://127.0.0.1:1317`
5. 等页面里的 `Status` 变成 `ready`
6. 发送 `Reply with exactly the single word ready. Do not call tools.`
7. 确认 Transcript 里出现 `ready`

如果你想直接验证 API，而不是页面：

创建 session：

```bash
curl -X POST http://127.0.0.1:1317/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{}'
```

发送 turn：

```bash
curl -X POST http://127.0.0.1:1317/api/sessions/<SESSION_ID>/turn \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"Reply with exactly the single word ready. Do not call tools."}'
```

查看状态：

```bash
curl http://127.0.0.1:1317/api/sessions/<SESSION_ID>/state
```

如果 transcript 里出现 `ready`，就说明 gateway、bridge 和 `codex app-server` 之间的链路已经跑通。

## 环境变量

gateway 自有配置统一使用 `CODEX_GATEWAY_` 前缀，便于和 Codex CLI 原生变量区分。

- `CODEX_GATEWAY_HOST`：Rust 服务监听地址，默认 `0.0.0.0`
- `CODEX_GATEWAY_PORT`：监听端口，默认 `1317`
- `CODEX_GATEWAY_CWD`：传给 `thread/start` 的工作目录，默认仓库根目录
- `CODEX_GATEWAY_CODEX_BIN`：`codex` 可执行文件路径，默认从 `PATH` 查找
- `CODEX_GATEWAY_MODEL`：新 bridge 默认模型
- `CODEX_GATEWAY_OPENAI_API_KEY`：启动时用于执行 `codex login --with-api-key` 的 API key
- `CODEX_GATEWAY_OPENAI_BASE_URL`：推荐使用的上游 OpenAI-compatible `base_url`。设置后，gateway 会把它配置成一个关闭 websocket 的自定义 Codex provider
- `CODEX_GATEWAY_MAX_SESSIONS`：最大同时在线 session 数，默认 `12`
- `CODEX_GATEWAY_SESSION_TTL_MS`：空闲 session TTL，默认 `1800000`
- `CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS`：清理扫描间隔，默认 `60000`
- `CODEX_GATEWAY_CODEX_HOME`：Codex 运行目录，包含认证缓存、日志、历史和配置；Docker 默认值是 `/codex-home`
- `CODEX_GATEWAY_DEBUG`：设为 `1` 时输出原始 bridge 消息，便于调试
- `CODEX_GATEWAY_JWT_SECRET`：可选的 HS256 JWT secret。设置后，除了 `/healthz` 和 `/readyz` 外，其他路由都需要合法的 Bearer token

## Docker

容器镜像会先构建 Rust gateway 二进制，再通过 `npm install -g @openai/codex` 在 Linux 中安装 Codex CLI，这和官方 Quickstart 一致。

构建镜像：

```bash
docker build -t codex-gateway .
```

运行容器：

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

说明：

- 如果设置了 `CODEX_GATEWAY_OPENAI_API_KEY`，容器会在启动 gateway 前自动执行 `codex login --with-api-key`
- `CODEX_GATEWAY_OPENAI_BASE_URL` 是把 Codex 指向第三方 OpenAI-compatible endpoint 的推荐方式；gateway 会把它映射成自定义 provider，而不是内建 `openai` provider
- 如果设置了 `CODEX_GATEWAY_JWT_SECRET`，普通 HTTP 请求需要带 `Authorization: Bearer <jwt>`；内置 Web UI 也支持在侧边栏直接填写 token
- 普通 API key 启动不需要挂载 `CODEX_GATEWAY_CODEX_HOME`；只有在你希望容器重启后保留 Codex 状态时才需要挂载
- 如果要让 Codex 在容器里操作别的工作目录，需要同时设置 `CODEX_GATEWAY_CWD` 并挂载对应路径
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
  -p 1317:1317 \
  -e CODEX_GATEWAY_OPENAI_API_KEY=sk-... \
  -e CODEX_GATEWAY_OPENAI_BASE_URL=https://sub2api-xnldrpuk.usw-1.sealos.app \
  -e CODEX_GATEWAY_HOST=0.0.0.0 \
  -e CODEX_GATEWAY_PORT=1317 \
  -e CODEX_GATEWAY_MAX_SESSIONS=8 \
  ghcr.io/che-zhu/codex-gateway:main
```

如果包可见性是私有的，拉取前需要先登录 GHCR。

## 当前限制

- 没有内建限流
- 没有持久化 session
- 没有审批 UI
- 每个活跃 session 都会占用一个 `codex app-server` 子进程
- 浏览器刷新后不会自动恢复原 session，除非调用方自己保存 `sessionId`
