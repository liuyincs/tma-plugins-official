# Subsonic / OpenSubsonic

只读 Subsonic / OpenSubsonic 适配器：把 TMA 的音乐服务以 `/rest/*` 暴露给
Subsonic 客户端，协议代码不进服务端核心。插件跑在 TMA 的 WASM 沙箱里，
经 `tma_http` 导出接收入站请求，目录、身份与流媒体分别走
`catalog.read`/`identity.read`/`media.stream` capability 宿主函数。

## 安装

本插件随 TMA Official 插件仓库分发：管理员在「插件管理 → 插件仓库」里
打开已预置的 TMA Official catalog，搜索 Subsonic 安装即可。官方包由仓库
CI 统一签名，服务端内置官方公钥，无需任何信任配置。

## 客户端认证

用户在「个人资料 → 外部客户端」创建凭据后，Subsonic 客户端任选一种方式
提交，认证全部由宿主完成（插件不接触凭据）：

- `Authorization: Bearer <secret>`
- `credential=<secret>` 查询参数
- Subsonic `p=<secret>`（含 `p=enc:<hex-secret>` 形式）
- OpenSubsonic `apiKey=<secret>`
- Subsonic 传统 `t`/`s`：按 `u` 定位用户后校验 `md5(secret+salt)`

`u`（用户名）为必填且须与凭据所属用户一致；认证参数在进入插件前已被
宿主剥离。

## 支持的端点

`ping`、`getMusicFolders`、`getArtists`、`getAlbum`、`getSong`、
`getCoverArt`、`stream`（`/rest/name` 与 `/rest/name.view` 两种拼写）。
默认返回 XML，客户端带 `f=json` 或含 `json` 的 `Accept` 头时返回 JSON。
`stream` 接受 `format`/`maxBitRate` 与 HTTP Range，转码、缓存与分段都交给
宿主。用户可见的边界见 [`compatibility.md`](compatibility.md)。

## 构建

目标为 `wasm32-unknown-unknown`，依赖 crates.io 的 `tma-plugin-sdk` 0.3：

```sh
cargo build --target wasm32-unknown-unknown --release
```
