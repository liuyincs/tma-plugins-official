# 插件模板

`plugin-template/` 是最小 `scrape_provider` 插件：自带 `[workspace]` 的独立
crate，复制即可单独构建。

```sh
cp -R templates/plugin-template plugins/<name>
cd plugins/<name>
cargo build --target wasm32-unknown-unknown --release
```

复制后的修改清单：

1. `manifest.json`：`id`（全局唯一，点分段，禁 `tma.builtin.` 前缀）、`name`、
   `scrape.provider`、`description`、`author`；`permissions` 用到什么声明什么
   （`reason` 必填）。`abi` 区间按所用 `tma-plugin-sdk` 文档核对——模板写的是
   `1.0 ..= 1.6`（当前宿主 ABI 上限），发布前以 SDK 声明的兼容区间为准。
2. `Cargo.toml`：`package.name`（`-` 转成 wasm 文件名里的 `_`）。
3. `src/lib.rs`：`slug` 换成你的 provider slug；`run_scrape` 是业务替换点。

签名、打包与发布流程见 [docs/authoring.md](../docs/authoring.md)。
