# CI/CD 与发布

## Pull Request 检查

`.github/workflows/ci.yml` 对每个 Pull Request 执行：

```bash
cargo fmt --all --check
cargo clippy --workspace
cargo test --workspace
```

在 GitHub 的默认分支保护规则中，将 `Workspace checks` 配置为 required status
check，才能真正阻止检查失败的 PR 合并。Workflow 本身不能修改仓库的分支保护设置。

## Tag 规范

版本必须与目标 package 的 `Cargo.toml` 中 `package.version` 完全一致。

统一使用：

```text
source-downloader-<name>-v<version>
```

Release workflow 优先用 `source-downloader-<name>` 匹配完整 package 名；找不到时，
再用 `<name>` 匹配短 package 名。因此当前 `web` package 使用
`source-downloader-web-v0.1.0`，`source-downloader-sdk` package 使用
`source-downloader-sdk-v0.1.0`。Workflow 根据 package 是否恰好包含一个 binary
target 自动区分 Application 与可发布 crate。

预发布版本使用 SemVer 后缀，例如 `v1.0.0-beta.1`。带预发布后缀的 GitHub
Release 会设置 `prerelease=true`；稳定版会设置为 `false`。

## Application 发布

推送 `source-downloader-*-v*` Tag 后，统一的 Release workflow 会识别
Application，并：

1. 从 Tag 解析 package 和版本，并与 workspace metadata 校验。
2. 校验 package 恰好包含一个 binary target。
3. 对三个目标分别执行 release build：
   `x86_64-pc-windows-msvc`、`x86_64-unknown-linux-gnu` 和
   `aarch64-unknown-linux-gnu`。
4. 将 binary 重命名为包含 package、版本和 target 的唯一文件名；Windows 保留
   `.exe`，不额外压缩。
5. 三个平台二进制全部成功后，再执行 Web 镜像阶段（仅 `web`）。
6. 使用 git-cliff 生成本次 Release Notes 和完整 `CHANGELOG.md`。
7. 创建 GitHub Release 并上传三个原始二进制文件与 `CHANGELOG.md`。

Release Notes 的提交范围从同一 release name 的上一个 Tag 开始，不会把另一个
package 的发布边界误用为当前 package 的发布边界。完整 CHANGELOG
作为每个 GitHub Release 的资产生成，不由 Tag workflow 向默认分支回写提交。

### Web Docker 镜像

当 package 为 `web` 时，同一 workflow 会在三个二进制平台均构建成功后，再构建
镜像；镜像内执行 `source-downloader-web --help` 冒烟验证，并以一个 manifest 推送
`linux/amd64`、`linux/arm64` 两个镜像平台。需要在 GitHub Actions
Variables/Secrets 中配置：

- `DOCKER_IMAGE_PREFIX`（必需）：包含 registry 和 namespace 的小写前缀，例如
  `ghcr.io/acme` 或 `docker.io/acme`。最终仓库为
  `<DOCKER_IMAGE_PREFIX>/web`。
- `DOCKER_REGISTRY_USERNAME`（可选）：registry 用户名；未设置时使用
  `github.actor`。
- `DOCKER_REGISTRY_TOKEN`（可选）：registry token；未设置时使用
  `GITHUB_TOKEN`。推送非 GHCR registry 时应显式设置。

所有版本都会推送 `<image>:<version>`。稳定版同时更新 `<image>:latest`；Beta
版本不会覆盖 `latest`。若发布 `web` 时未配置有效的 `DOCKER_IMAGE_PREFIX`，
workflow 会失败，而不是静默跳过镜像发布。

## Crate 发布

统一的 Release workflow 识别到无 binary target 的可发布 crate 后，会校验 package、
版本和 `publish = false`，然后执行：

```bash
cargo publish --locked --package <package>
```

仓库必须配置 Actions secret `CARGO_REGISTRY_TOKEN`。发布成功后 workflow 创建
GitHub Release，并附带 git-cliff 生成的 Release Notes 和完整 `CHANGELOG.md`。

Cargo 要求所有将上传 crates.io 的 path dependency 同时声明 registry version，
且 crate metadata 满足 crates.io 的打包规则。创建 Tag 前先执行文末的 dry-run；
该检查失败时不要推送 Tag。SDK 应先发布，依赖 SDK 的 Core 再发布，各自版本无需相同。

当前 workspace 尚未满足首次 crates.io 发布前提：`source-downloader-sdk` 对
`component-macro` 的 path dependency 未声明 version，Core 对 SDK 的 path
dependency 也未声明 version。受本次任务的文件修改范围限制，Cargo manifests
未改动。首次发布前必须为这些依赖补充与已发布 crate 一致的 version，并按
`component-macro` → SDK → Core 的顺序 dry-run 和发布；否则 Cargo 会在上传前拒绝
manifest。

## Conventional Commits 与 CHANGELOG

提交信息采用 Conventional Commits，例如：

```text
feat(core): add component lifecycle
fix(web): fix login issue
feat(sdk)!: change component api
```

`cliff.toml` 的分类规则为：

- `feat` → Added
- `refactor`、`perf`、`docs`、`revert` → Changed
- `fix` → Fixed
- `!` 或 `BREAKING CHANGE` → Breaking Changes
- `build`、`chore`、`ci`、`style`、`test` → 不进入发布说明

## 发布命令示例

先更新目标 package 的版本并合并通过 CI 的提交，再在该提交创建 Tag：

```bash
# 当前 Web application 稳定版
git tag source-downloader-web-v0.1.0
git push origin source-downloader-web-v0.1.0

# 独立 SDK 稳定版
git tag source-downloader-sdk-v0.1.0
git push origin source-downloader-sdk-v0.1.0

# package 版本已经更新为 1.0.0-beta.1 时发布 Beta
git tag source-downloader-web-v1.0.0-beta.1
git push origin source-downloader-web-v1.0.0-beta.1
```

## 本地验证

```bash
cargo fmt --all --check
cargo clippy --workspace
cargo test --workspace

# 发布 crate 前验证打包；替换 package
cargo publish --locked --package source-downloader-sdk --dry-run

# 安装 git-cliff 后预览完整 CHANGELOG
git cliff --config cliff.toml
```

可用 `cargo metadata --no-deps --format-version 1` 检查准确的 package 名称、版本和
binary targets。Docker 发布可在本地使用与 workflow 相同的多阶段 Dockerfile
逻辑执行 `docker build`；registry 登录和推送凭据只在 GitHub Actions 中配置。

## 新增 Application

1. 将新的 application crate 加入 workspace，并设置唯一的 `package.name` 和版本。
2. 恰好声明一个 binary target。
3. 合并后创建 `source-downloader-<name>-v<version>` Tag；`<name>` 可以是完整
   package 名去掉 `source-downloader-` 后的短名称。

通用二进制构建、重命名、按 package 计算 Release Notes 范围和 GitHub Release
步骤都不需要修改。只有 `web` 启用当前的 Docker 镜像分支；未来其他 application
需要镜像时，可将镜像构建条件扩展为配置化 package 列表，而无需复制发布 workflow。
