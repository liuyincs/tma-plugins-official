# plugins/

每个子目录是一个官方插件（独立 workspace，从 `templates/plugin-template` 复制起步）。

新增插件：把 `templates/plugin-template` 复制为 `plugins/<name>` 后即可——
CI 由 `scripts/discover-plugins.py` 从本目录一级非隐藏子目录自动发现插件并做
契约校验，无需手改 `.github/workflows/test.yml`。契约是 fail-closed 的：目录名
限 `[A-Za-z0-9_-]`，目录须同时含 `manifest.json` 与 `Cargo.toml`，`id` 全局唯一
且符合 `docs/authoring.md` 的格式规则；任一目录不合规会让整个 discover job
失败。本节下方的插件目录清单仍由人工维护，记得顺手补上。

## 插件目录

- `spotify`：Spotify Client Credentials 艺术家 ID 检索
- `apple_music`：Apple Music 目录搜索艺术家 ID
- `fanart_tv`：Fanart.tv 艺术家/专辑套图
- `listenbrainz`：ListenBrainz scrobble / now playing 上报（非刮削来源）
- `lastfm`：Last.fm 简介/图片/外部 ID + scrobble 上报与网页授权
- `subsonic`：Subsonic/OpenSubsonic 只读适配器（`/rest/*` 入站 HTTP）

发布约定：tag `<目录名>-v<semver>`（如 `template-v0.1.0`）触发 CI 构建、签名、
创建 GitHub Release 并把条目回写 `catalog.json`（见
`../.github/workflows/release.yml` 与 `../docs/authoring.md`）。

各插件 `tests/` 内是行为级验收测试（构建 wasm → 测试私钥打包验签 → stub
出站驱动真实 wasm），共享件在仓库根的 `testkit/`；插件目录内 `cargo test`
跑全量（含 scrobble 纯逻辑单测）。
