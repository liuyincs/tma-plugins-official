//! 模板插件：最小 `scrape_provider` 实现（返回固定占位结果）。
//!
//! 复制本目录为新插件后需要修改：
//! - `manifest.json`：`id`/`name`/`scrape.provider`/`description`/`author`/`permissions`；
//! - `Cargo.toml`：`package.name`（决定 wasm 产物文件名：`-` 转 `_` + `.wasm`）；
//! - 本文件：`slug` 与 `run_scrape` 业务逻辑。

// extism-pdk 引用的宿主函数（alloc/free 等）只在 wasm target 存在，
// 宿主编译必然链接失败，故整 crate 仅在 wasm32 下启用。
#![cfg(target_arch = "wasm32")]

// 顶层 re-export 即插件 API DTO；`plugin!` 宏展开时已注入
// `use tma_plugin_sdk::__rt::extism_pdk;`。若手写 host_fn 声明等需要直接
// 使用 extism-pdk 的代码，经 `tma_plugin_sdk::__rt::extism_pdk` 引入即可，
// 插件无需（也不应）直接依赖 extism-pdk。
use tma_plugin_sdk::{EntityQuery, FetchedId, PluginError, ScrapeResult};

// FFI 样板（manifest 导出 / host_fn 声明 / scrape 分发 / transport）由宏吐出。
tma_plugin_sdk::plugin! {
    manifest = "../manifest.json",
    slug = "template",
    scrape = run_scrape,
}

/// 单实体刮削入口。
///
/// 占位实现：固定回一条 `external_ids`，用于验证「安装 → 探测 → 调用」链路；
/// 占位数据会真实落库，投入业务前必须替换。
/// 真实实现在此按 `query`（`kind`/`name`/`mbid`/`isrc`/`known_external_ids`）
/// 组装请求，经宏生成的 `transport()` 出站 HTTP（目标 host 须已在
/// `manifest.json` 的 `permissions` 声明），解析响应填充 `ScrapeResult`；
/// 无结果返回 `Ok(ScrapeResult::default())`。
fn run_scrape(query: EntityQuery) -> Result<ScrapeResult, PluginError> {
    let _ = query;
    Ok(ScrapeResult {
        external_ids: vec![FetchedId {
            provider: "template".into(),
            external_id: "template-placeholder".into(),
            url: None,
        }],
        confidence: 0.5,
        ..Default::default()
    })
}
