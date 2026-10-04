# TMA 插件作者指南

TMA 插件是 wasm32 模块（extism 沙箱），经 ed25519 签名打成 `.tmap` 包分发；
宿主默认拒绝一切未声明的副作用。本文覆盖从模板到发布的完整流程。

## 前置

```sh
rustup target add wasm32-unknown-unknown
cargo install tma-plugin-dev --locked   # 子命令 keygen / pubkey / pack
```

## 从模板起步

```sh
cp -R templates/plugin-template plugins/<name>
cd plugins/<name>
cargo build --target wasm32-unknown-unknown --release
```

各插件目录是独立 workspace（自带 `[workspace] members = ["."]`），`target/`
落在插件目录内。复制后的修改清单见 [templates/README.md](../templates/README.md)。

## SDK 与 FFI

业务代码只依赖 `tma-plugin-sdk`。一个宏吐出全部 FFI 样板（manifest 导出、
`http_request` 宿主函数声明、scrape 入口的 op 分发、`transport()` 出站闭包），
插件只写业务函数：

```rust
#![cfg(target_arch = "wasm32")]   // 必需：宿主 target 下链接必然失败

tma_plugin_sdk::plugin! {
    manifest = "../manifest.json", // include_str! 相对调用文件
    slug = "example",              // 仅用于 Unsupported 文案
    scrape = run_scrape,           // 可选：fn(EntityQuery) -> Result<ScrapeResult, PluginError>
    // 可选片段（按需要追加）：
    // browse = f,   // 碟谱 browse：fn(&str) -> Result<Vec<ScrapedOfficialAlbum>, PluginError>
    // config = f,   // 声明 tma_config 宿主函数并生成指定名的配置读取助手
    // actions = f,  // 生成 tma_action 导出（ABI 1.2 起）
    // event = f,    // 生成 tma_event 导出（ABI 1.3 起）
    // import = f,   // 生成 tma_playlist_import 导出（ABI 1.4 起）
    // ai_chat,      // 声明 ai_chat 宿主函数（与 import 独立，须写在最后）
}
```

未声明 `scrape` 时仍会导出 `scrape`（宿主 ABI 探测需要），业务恒
`Unsupported`。extism-pdk 不经插件直接依赖：宏展开会自动注入
`use tma_plugin_sdk::__rt::extism_pdk;`，手写 `host_fn` 声明等场景也
用 `tma_plugin_sdk::__rt::extism_pdk` 引入。

## manifest.json 字段

宿主加载前的第一道判定依据；`.tmap` 内的原始字节同时是验签输入。

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `id` | 是 | 全局唯一：`[A-Za-z0-9_]` 按点分段，≤200 字符，禁 `-`/空格/空段；`tma.builtin.` 前缀保留给官方内置播种，手动安装/第三方分发不可用 |
| `name` | 是 | 显示名 |
| `version` | 是 | 规范 SemVer；catalog 展示与升级判定都按它 |
| `abi` | 是 | 兼容的宿主 ABI 闭区间 `{min, max}`（当前宿主 ABI 1.6）。部分字段有最低 ABI 门槛，见下 |
| `extension_points` | 是 | `scrape_provider` / `scrobble_reporter` / `playlist_import` / `http` |
| `permissions` | 否 | 权限声明，缺省 = 无任何副作用能力（见下节） |
| `scrape` | 条件 | 声明 `scrape_provider` 时必填：`provider`（落库来源 slug，与 `FetchedId.provider` 一致）、`capabilities`（`external_ids`/`bio`/`image`）、`requires_credentials`（`true` 时必须同时声明非空 `config_secrets`，宿主按 secrets 是否已配置决定注册）、`library_types`（缺省 `["music"]`；显式声明要求 `abi.min ≥ 1.5`） |
| `config_schema` / `config_secrets` | 否 | 管理面配置表单（受限 JSON Schema）；secrets 按点分路径声明，值加密落库且接口不回显 |
| `user_config_schema` / `user_config_secrets` | 否 | 每用户配置（`abi.min ≥ 1.3`），结构同全局配置 |
| `actions` | 否 | 插件动作 `[{id, label}]`（`abi.min ≥ 1.2`，配合 `tma_action` 导出）；label 字面量或 `$` 前缀 i18n 引用（约定键 `actions.<id>.label`） |
| `scrobble_reporter` | 条件 | 声明 `scrobble_reporter` 时必填（`abi.min ≥ 1.3`）：`events` 非空子集 `scrobble`/`now_playing` |
| `playlist_import` | 条件 | 声明 `playlist_import` 时必填（`abi.min ≥ 1.4`）：`methods` 非空 `[{id, label, input_schema?, requires_ai}]` |
| `http` | 条件 | 声明 `http` 扩展点时必填（`abi.min ≥ 1.6`）：`routes: [{path, methods[]}]`，`path` 末尾 `/*` 捕获剩余路径，配 `tma_http` 导出 |
| `i18n` | 否 | `{locale: {key: text}}`；保留键 `name`/`description` 覆盖顶层文案，`$` 引用须能被 `i18n.en` 解析 |
| `description` / `author` / `homepage` | 否 | catalog 展示元数据 |

### permissions

外标签枚举，三种形态：

```json
{"http": {"scheme": "https", "host": "api.example.com", "reason": "为什么需要"}}
{"ai": {"reason": "..."}}
{"capability": {"name": "catalog.read", "reason": "..."}}
```

- `reason` 必填非空——在安装确认界面原样展示给用户；
- `http.host` 写裸 host（不得含 `://` 或路径）：`example.com` 精确匹配、
  `*.example.com` 任意深层子域（不含裸域）、`example.com:8080` 精确端口；
  `scheme` 缺省 `https`；可选 `traffic` 声明配额组由宿主统一限速；
- `ai`（`abi.min ≥ 1.4`）：宿主单轮 AI 对话，凭据是服务端共享资源；
- `capability.name`（`abi.min ≥ 1.6`）当前限 `catalog.read` / `identity.read` /
  `media.stream`，实际请求仍经宿主 DTO 逐次校验；
- 未声明的请求宿主逐次拒绝（默认拒绝，不是安装期一次性判定）。

## 验收测试

`tests/` 放行为级验收测试：`testkit`（仓库根共享件）负责构建 wasm、测试私钥
打包验签、extism 实例化与 stub 出站代理——测试直接调 `scrape`/`tma_action`/
`tma_event` 导出断言请求形状与响应语义。写法见 `plugins/spotify/tests/`
与 `testkit/`；`cargo test` 在插件目录内跑全量（含双 target 纯逻辑单测）。
CI 在发布签名前强制执行（`.github/workflows/test.yml` 与 `release.yml`）。

## 签名与打包

`.tmap` 是 STORED ZIP：`manifest.json` + `plugin.wasm` + `SIGNATURE.sig`
（+ 可选 `icon.svg`/`icon.png`，二选一）+ ed25519 detached 签名，输入为
`manifest 原始字节 ‖ sha256(plugin.wasm)〔‖ sha256(icon)〕`。格式与验签细节
见 TMA 主仓库文档 `docs/plugins/official-signing.md`。

```sh
tma-plugin-dev keygen                    # 生成密钥对：私钥保密，公钥即分发身份
export TMA_PLUGIN_SIGNING_KEY_B64=<私钥b64>   # 私钥走环境变量，不进 argv/进程列表
tma-plugin-dev pubkey                    # 从私钥导出公钥（也支持 --key-file PATH，- 为 stdin）
tma-plugin-dev pack <插件目录> -o out.tmap
# <插件目录> 须含 manifest.json 与 plugin.wasm
#（把 target/wasm32-unknown-unknown/release/<crate>.wasm 复制为插件目录下
#  plugin.wasm 再 pack；存在 icon.svg/icon.png 时自动打入）
```

本仓库的 `scripts/build-package.sh` 封装了复制/pack/sha256/清理全流程。

## 在本仓库发布（官方插件）

tag `<dir>-v<semver>`（`<dir>` 为 `plugins/` 下目录名，如 `template-v0.1.0`）
触发 `.github/workflows/release.yml`：构建 wasm → `cargo install
tma-plugin-dev --locked` → 用 secret `TMA_PLUGIN_SIGNING_KEY_B64` 签名打包 →
`gh release create` 上传 `dist/<name>.tmap`/`.sha256` →
`scripts/update-catalog.py` 把条目回写 `catalog.json` 并提交回 main。

前提：GitHub secret `TMA_PLUGIN_SIGNING_KEY_B64` 已配置官方生产签名私钥
（绝不入库，人手不接触）。`release.yml` 在每次发布时用 `tma-plugin-dev pubkey`
从该私钥派生 `public-key.txt` 并随 `catalog.json` 一并提交——官方公钥已编译进
服务端（用户安装官方插件零配置），该文件用于 catalog 条目的
`public_key_url`/`public_key_fingerprint` 展示与外部校验参考。

## 自建 catalog（第三方分发）

静态 `catalog.json` 顶层 `{name, plugins[]}`，条目字段与
`scripts/update-catalog.py` 的产物一致：`id`、`name`、`version`（SemVer）、
`description`、`author`、`homepage`、`abi`、`permissions`、`download_url`、
`sha256`、`public_key_url`、`public_key_fingerprint`。其中
`public_key_fingerprint` 的约定口径为
`"sha256:" + sha256hex(base64decode(public-key.txt 内容))`。

服务端解析时只强校验 `id` 非空、`download_url` 非空、`version` 为合法
SemVer；其余字段用于展示与验签参考，未知字段忽略。用户在「插件仓库」里
添加 catalog 的 http(s) URL 即可浏览安装。

第三方插件的用户侧信任：服务端环境变量 `TMA_PLUGIN_PUBKEYS`（逗号分隔的
base64 ed25519 公钥）在编译内置的官方公钥之外**追加**信任，不做替换；
验签失败的包一律拒绝安装/加载。发布第三方插件时把 `tma-plugin-dev pubkey`
导出的公钥随包分发（如仓库根 `public-key.txt`），引导用户配置。
