# Release Format

本文档约束本仓库后续 GitHub Release 的命名、正文结构、发布产物和校验说明格式。

## Version Tags

正式 release 必须使用 `vX.Y.Z` 格式，例如 `v0.5.1`。

发布前需要同步更新版本号：

- `Cargo.toml` 的 `package.version`
- `package.json` 的 `version`

Git tag 应使用 annotated tag，tag message 使用：

```text
Release vX.Y.Z
```

## Release Title

正式 release 标题使用 tag 本身：

```text
vX.Y.Z
```

例如：

```text
v0.5.1
```

## Release Body

正式 release 正文必须使用以下四个二级标题，并保持顺序不变：

```md
## Changes

- ...

## Release assets

- `codex-gateway-darwin-arm64.tar.gz`
- `codex-gateway-darwin-arm64.tar.gz.sha256`
- `codex-gateway-linux-amd64.tar.gz`
- `codex-gateway-linux-amd64.tar.gz.sha256`

## Container image

- `ghcr.io/labring/codex-gateway:latest`
- `ghcr.io/labring/codex-gateway:vX.Y.Z`

## Validation

- ...
```

## Changes

`Changes` 部分用 bullet list 描述本次 release 的用户可见变化和重要内部变化。

要求：

- 每条变更使用一句完整说明。
- 优先写行为变化、API 变化、运行时变化、镜像变化、兼容性变化。
- 不需要逐条复制 commit message。
- 如果包含 breaking change，需要在对应 bullet 中明确说明。

## Release Assets

正式 release 应上传以下 assets：

- `codex-gateway-darwin-arm64.tar.gz`
- `codex-gateway-darwin-arm64.tar.gz.sha256`
- `codex-gateway-linux-amd64.tar.gz`
- `codex-gateway-linux-amd64.tar.gz.sha256`

正文中的 asset 文件名必须使用反引号包裹，并与实际上传文件名完全一致。

## Container Image

正式 release 正文至少列出以下镜像：

- `ghcr.io/labring/codex-gateway:latest`
- `ghcr.io/labring/codex-gateway:vX.Y.Z`

推送 `vX.Y.Z` tag 后，GitHub Actions 会额外发布 semver 相关镜像 tag：

- `X.Y.Z`
- `X.Y`
- `X`
- `latest`

如需在 `Changes` 中说明镜像发布范围，可以写明完整 tag 集合，例如：

```md
- Publish GHCR image tags for `v0.5.1`, `0.5.1`, `0.5`, `0`, and `latest`.
```

## Validation

`Validation` 部分必须列出本次 release 实际完成的校验和构建。

常见条目包括：

- `cargo fmt --check` passed.
- `cargo check` passed.
- `cargo test` passed.
- Darwin arm64 release binaries were built locally and packaged with the static web assets.
- Linux amd64 release binaries were built from the repository Docker builder target and packaged with the static web assets.
- GitHub Actions Container workflow passed for `vX.Y.Z`.

只写实际执行过并通过的检查。不要把未执行的检查写进 release notes。

## Prereleases

预发布可以使用描述性 tag，例如：

```text
rust-preview-YYYYMMDD-N
```

预发布标题可以使用可读名称，例如：

```text
Rust preview YYYY-MM-DD
```

预发布正文可以使用自由格式，但必须说明：

- 对应 commit 或日期。
- 包含哪些 assets。
- 目标平台。
- 重要运行时要求。
- 与正式 release 的差异。

## Historical Notes

本仓库历史格式演进如下：

- `v0.2.0` 使用 GitHub 自动生成 release notes，没有固定四段结构，也没有二进制 assets。
- `rust-preview-20260410-1` 是 macOS arm64 Rust 预览包，使用自由格式并标记为 prerelease。
- 从 `v0.3.0` 起，正式 release 使用 `Changes`、`Release assets`、`Container image`、`Validation` 四段结构。
- `v0.5.1` 是当前推荐格式基准：asset 和 image 均使用反引号，`Validation` 明确列出测试和各平台构建来源。
