//! TMA 官方插件的验收测试共享件（`testkit`）。
//!
//! 用法：在插件目录的 `tests/` 里经 [`HostSim`] 装载、[`Driver`] 驱动——
//! 这是插件测试的唯一接口：
//!
//! ```ignore
//! let proxy = StubProxy::new(vec![RouteBuilder::json("api.example.com", json!({...}))]);
//! let plugin = HostSim::new()
//!     .config(r#"{"api_key":"k"}"#)
//!     .proxy(proxy.clone())
//!     .load_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
//! let r = plugin.call_scrape(testkit::artist_query(None, Some("X"))).unwrap();
//! ```
//!
//! 三层角色：
//!
//! - [`HostSim`]（模块 `hostsim`）：装载入口。builder 式收拢注入面（运行时
//!   config JSON、stub 代理、capability 桩），`load_dir`（构建 → pack →
//!   verify → 实例化，夹具失败 panic）/ `load_verified`（Result 语义）/
//!   `load_wasm`（裸 wasm，无清单权限门全放行）产出 [`Driver`]；
//! - [`Driver`]（模块 `driver`，原 `LoadedPlugin`）：调用驱动。
//!   `call_scrape`/`call_action`/`call_event`/`call_http` 驱动插件导出，
//!   instance-per-call（每次调用新实例，宿主同语义）；`http_requests`/
//!   `catalog_requests` 等观察方法读出跨调用累计的请求留痕；
//! - `contract` 模块（测试态）：契约夹具回放。`contract/capability_host.json`
//!   逐行手工转写自宿主 `capability_fns.rs`，回放器直接打真实运行的
//!   `catalog_respond`/`identity_respond`/`media_respond`，校验顺序与
//!   错误码偏离宿主源时测试显红。
//!
//! 测试仍直接构造 [`StubProxy`]/[`CapabilityStub`] 注入（adapter 配置面）；
//! 插件实例只能由 [`HostSim`] 造。
//!
//! `load_dir` 走「真实签名包」链路：`cargo build --target wasm32-unknown-unknown
//! --release`（增量缓存）→ 测试私钥 `pack` → `verify` 验签 → extism 实例化，
//! 与宿主安装路径同源（`tma-plugin-sdk` 的 `package` feature 即打包/验签实现）。
//!
//! 与真实宿主（`tma-plugin-host`）的保真边界：
//!
//! - `http_request` 宿主函数（`extism:host/user`）：入参/出参按
//!   `tma_plugin_sdk::http_host` 的 `HttpHostRequest`/`HttpHostResponse` JSON
//!   协议逐字段复现——i64 offset → JSON 入参 → `body_b64` 解码 → 响应
//!   `status`/`headers`/`body_b64` 编码 → 写回 offset；panic 兜底为
//!   `ok:false` 响应而非 trap；
//! - `tma_config` 宿主函数：无参，原样返回调用方注入的 config JSON 字符串；
//! - `tma_catalog_read`/`tma_identity_read`/`tma_media_stream` 宿主函数
//!   （ABI 1.6 capability）：与 `http_request` 同一 offset/JSON 往返协议，
//!   校验顺序与宿主 `capability_fns.rs` 逐分支一致（失败都写回结构化错误
//!   DTO，不 trap）。三条函数的顺序各不相同，以宿主源为准：catalog 是
//!   入参 → DTO 版本 → `catalog.read` 权限门 → `limit` 范围（权限门先于
//!   limit）；identity 是入参 → DTO 版本 → `identity.read` 权限门；media
//!   是入参 → DTO 版本 → `media_id` 非空 → range 合法 → `media.stream`
//!   权限门（字段校验先于权限门）。契约夹具（`contract` 模块）锁死这条顺序。
//!   注入面是 [`CapabilityStub`]——catalog/media 走「字段匹配 → 响应」
//!   路由表（首个命中优先），identity 是单值槽；进入桩派发的每次调用都
//!   先记录完整请求 DTO 再匹配。刻意的差异：桩未配置或未命中（即「夹具
//!   没有为该请求编排响应」）以宿主函数级 `Err` 失败（整个导出调用当场
//!   失败），保证测试不会把桩缺配置误读成已配置的业务失败——宿主侧对应
//!   的 provider-未注入语义是 `unavailable` DTO，testkit 的桩恒存在；
//! - `tma_http` 入站 HTTP 导出经 [`Driver::call_http`] 驱动：
//!   `PluginHttpRequest` JSON 进、`PluginHttpResponse` JSON 出；无导出 →
//!   `Unsupported`，业务 `Err` 由插件侧折叠为非零退出码（与
//!   `tma_playlist_import` 同口径），宿主侧只见调用失败；
//! - 签名包路径（`load_dir`/`load_verified`）按 manifest `permissions`
//!   复现逐次 capability 权限门：从 `verified.manifest` 提取声明过的
//!   capability 名集合，未声明的 capability 写回 `ok:false` + `forbidden`
//!   应答（media 为 403），不 trap——插件侧收到与真实宿主相同的可处理
//!   错误；`load_wasm` 无清单路径保持全放行；
//! - **刻意不复现**：`http_request` 出站的 manifest `http` 权限 URL 白名单
//!   （由 stub 代理的路由表承担「放行什么」）、真实认证
//!   （`PluginHttpRequest.identity` 与 `tma_identity_read` 应答都由测试
//!   注入）、媒体文件读取与 Range 字节切片/转码（media 桩返回什么就透传
//!   什么）、3xx 重定向跟随、fuel/epoch 打断——stub 代理是唯一的出站
//!   注入点，未命中路由直接以 `code:"network"` 显式报错（与宿主
//!   `ProxyHttpError::Network` 同码同文）。
//!
//! `TEST_SIGNING_KEY_B64` 是测试专用密钥对（生成后即冻结）。它只服务本仓
//! 验收测试的自洽链路，**不是任何环境的真实签名私钥**，也绝不应被配置为
//! 宿主受信公钥。

mod capability_stub;
#[cfg(test)]
mod contract;
mod driver;
mod host_fns;
mod hostsim;
mod package;
mod proxy;
mod query;

// 测试面直接消费的 SDK 类型统一从 testkit 再导出：插件测试只依赖
// `use testkit::{...}`，不直依 `tma_plugin_sdk`。全仓插件与 testkit 同在
// SDK 0.3 版本线（INT-5 收拢），断言口径即以 testkit 侧版本为准。
pub use tma_plugin_sdk::{
    CAPABILITY_DTO_VERSION, CapabilityError, CatalogItem, CatalogReadRequest, CatalogReadResponse,
    EntityQuery, FetchedId, HttpHostRequest, HttpHostResponse, IdentityReadRequest,
    IdentityReadResponse, LibraryType, MediaByteRange, MediaClientCapabilities, MediaStreamRequest,
    MediaStreamResponse, NowPlayingEventPayload, PluginActionRequest, PluginActionResponse,
    PluginError, PluginErrorCode, PluginEventKind, PluginEventRequest, PluginEventResponse,
    PluginHttpIdentity, PluginHttpRequest, PluginHttpResponse, PluginIcon, PluginManifest,
    PluginOp, PluginOutcome, PluginRequest, PluginResponse, ScrapeEntityKind, ScrapeResult,
    ScrobbleEventPayload, TrackEventFields, VerifiedPlugin, base64_decode, base64_encode,
};

pub use crate::capability_stub::{
    CapabilityStub, CatalogMatch, MediaMatch, catalog_err, catalog_item, catalog_ok, identity_err,
    identity_ok, media_err, media_ok,
};
pub use crate::driver::Driver;
pub use crate::hostsim::HostSim;
pub use crate::package::{
    TEST_SIGNING_KEY_B64, ensure_wasm_built, pack_dir, test_signing_key, test_verifying_key, verify,
};
pub use crate::proxy::{RecordedRequest, Route, RouteBuilder, StubProxy};
pub use crate::query::{
    ABBEY_RELEASE_MBID, ABBEY_RG_MBID, BEATLES_MBID, artist_query, artist_query_with_known,
};
