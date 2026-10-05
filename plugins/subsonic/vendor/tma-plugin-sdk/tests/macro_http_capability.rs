//! `plugin!` ABI 1.6 新槽的展开覆盖：`http` 导出槽 + `catalog_read` /
//! `identity_read` / `media_stream` capability 宿主函数槽。
//!
//! 宏展开的 `__rt::extism_pdk` 只在 wasm32 存在：宿主机 `cargo test` 下本文件
//! 编译为空；展开验证走
//! `cargo check -p tma-plugin-sdk --target wasm32-unknown-unknown --all-targets`
//! （check-only：同一 crate 内两次展开会重复导出 `tma_manifest`/`scrape`，
//! 不追求链接产物）。
#![cfg(target_arch = "wasm32")]

use tma_plugin_sdk::{
    CatalogReadRequest, CatalogReadResponse, HttpHostRequest, HttpHostResponse,
    IdentityReadRequest, IdentityReadResponse, MediaStreamRequest, MediaStreamResponse,
    PluginError, PluginHttpRequest, PluginHttpResponse,
};

// 全新槽（无 scrape/ai_chat，走第二匹配臂）。
tma_plugin_sdk::plugin! {
    manifest = "../fixtures/manifest.http.json",
    slug = "subsonic",
    http = run_http,
    catalog_read = catalog,
    identity_read = identity,
    media_stream = media_stream,
}

fn run_http(_req: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
    Ok(PluginHttpResponse {
        status: 204,
        headers: Default::default(),
        body_b64: None,
    })
}

// 以函数指针常量钉住生成助手的签名（顺带消除 dead_code）。
const _: fn(HttpHostRequest) -> Result<HttpHostResponse, PluginError> = transport;
const _: fn(CatalogReadRequest) -> Result<CatalogReadResponse, PluginError> = catalog;
const _: fn(IdentityReadRequest) -> Result<IdentityReadResponse, PluginError> = identity;
const _: fn(MediaStreamRequest) -> Result<MediaStreamResponse, PluginError> = media_stream;

// `ai_chat` 开关与 capability 槽共存（走第一匹配臂；capability 槽仍在 ai_chat 之后）。
mod with_ai_chat {
    use tma_plugin_sdk::{
        CatalogReadRequest, CatalogReadResponse, HttpHostRequest, HttpHostResponse,
        IdentityReadRequest, IdentityReadResponse, MediaStreamRequest, MediaStreamResponse,
        PluginError,
    };

    tma_plugin_sdk::plugin! {
        manifest = "../fixtures/manifest.http.json",
        slug = "subsonic_ai",
        ai_chat,
        catalog_read = catalog,
        identity_read = identity,
        media_stream = media_stream,
    }

    const _: fn(HttpHostRequest) -> Result<HttpHostResponse, PluginError> = transport;
    const _: fn(CatalogReadRequest) -> Result<CatalogReadResponse, PluginError> = catalog;
    const _: fn(IdentityReadRequest) -> Result<IdentityReadResponse, PluginError> = identity;
    const _: fn(MediaStreamRequest) -> Result<MediaStreamResponse, PluginError> = media_stream;
}
