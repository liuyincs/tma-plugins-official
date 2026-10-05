# plugins/

每个子目录是一个官方插件（独立 workspace，从 `templates/plugin-template` 复制起步）。

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
