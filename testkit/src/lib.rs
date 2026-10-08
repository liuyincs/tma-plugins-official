//! TMA 官方插件的验收测试共享件（`testkit`）。
//!
//! 用法：在插件目录的 `tests/` 里
//!
//! ```ignore
//! let proxy = StubProxy::new(vec![RouteBuilder::json("api.example.com", json!({...}))]);
//! let mut plugin = testkit::load_dir(
//!     std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
//!     r#"{"api_key":"k"}"#,
//!     proxy.clone(),
//! );
//! let r = plugin.call_scrape(testkit::artist_query(None, Some("X"))).unwrap();
//! ```
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
//!   校验顺序与宿主 `capability_fns.rs` 逐字段一致（入参/版本/字段/
//!   权限门失败都写回结构化错误 DTO，不 trap）。注入面是
//!   [`CapabilityStub`]——catalog/media 走「字段匹配 → 响应」路由表
//!   （首个命中优先），identity 是单值槽；进入桩派发的每次调用都先记录
//!   完整请求 DTO 再匹配。刻意的差异：桩未配置或未命中（即「夹具没有为
//!   该请求编排响应」）以宿主函数级 `Err` 失败（整个导出调用当场失败），
//!   保证测试不会把桩缺配置误读成已配置的业务失败——宿主侧对应的
//!   provider-未注入语义是 `unavailable` DTO，testkit 的桩恒存在；
//! - `tma_http` 入站 HTTP 导出经 [`LoadedPlugin::call_http`] 驱动：
//!   `PluginHttpRequest` JSON 进、`PluginHttpResponse` JSON 出；无导出 →
//!   `Unsupported`，业务 `Err` 由插件侧折叠为非零退出码（与
//!   `tma_playlist_import` 同口径），宿主侧只见调用失败；
//! - instance-per-call：每次导出调用都新建 extism 实例（宿主同语义）；
//! - 签名包路径按 manifest `permissions` 复现逐次 capability 权限门：
//!   `from_verified_with_capabilities` 从 `verified.manifest` 提取声明过的
//!   capability 名集合；校验顺序与宿主 `capability_fns.rs` 一致（入参 →
//!   DTO 版本 → 字段校验 → 权限门 → 派发），未声明的 capability 写回
//!   `ok:false` + `forbidden` 应答（media 为 403），不 trap——插件侧收到
//!   与真实宿主相同的可处理错误；`from_wasm*` 无清单路径保持全放行；
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

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};
use extism::{CurrentPlugin, Function, UserData, Val, ValType};
// 测试面直接消费的 SDK 类型统一从 testkit 再导出：插件测试只依赖
// `use testkit::{...}`，不直依 `tma_plugin_sdk`——插件 crate 自身的 SDK 版本
// 可以与 testkit 的不同（DTO 线格式逐字节兼容，断言口径以 testkit 侧为准）。
pub use tma_plugin_sdk::{
    CAPABILITY_DTO_VERSION, CapabilityError, CatalogItem, CatalogReadRequest, CatalogReadResponse,
    EntityQuery, FetchedId, HttpHostRequest, HttpHostResponse, IdentityReadRequest,
    IdentityReadResponse, MediaByteRange, MediaClientCapabilities, MediaStreamRequest,
    MediaStreamResponse, PluginActionRequest, PluginActionResponse, PluginError, PluginErrorCode,
    PluginEventRequest, PluginEventResponse, PluginHttpIdentity, PluginHttpRequest,
    PluginHttpResponse, PluginIcon, PluginManifest, PluginOp, PluginOutcome, PluginRequest,
    PluginResponse, ScrapeEntityKind, ScrapeResult, VerifiedPlugin, base64_decode, base64_encode,
};

// ---------------------------------------------------------------------------
// stub 代理：URL 子串匹配的罐头响应 + 请求记录
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

/// 单条路由：URL 含 `needle` 即命中（首个命中优先）。
pub struct Route {
    pub needle: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub struct RouteBuilder;

impl RouteBuilder {
    /// 200 + JSON body 的罐头响应（等价原 `builtin_tests::support::RouteBuilder::json`）。
    pub fn json(needle: &str, body: serde_json::Value) -> Route {
        Route {
            needle: needle.to_string(),
            status: 200,
            headers: BTreeMap::new(),
            body: body.to_string(),
        }
    }
}

/// 记录型 stub 代理：未命中路由 → `network` 失败（测试将失败并带出 URL，
/// 与宿主 `ProxyHttpError::Network` 同码同文）。
pub struct StubProxy {
    routes: Vec<Route>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl StubProxy {
    pub fn new(routes: Vec<Route>) -> Arc<Self> {
        Arc::new(Self {
            routes,
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.url).collect()
    }

    /// `http_request` 宿主函数的业务段：先记录再匹配（未命中也留痕）。
    fn dispatch(
        &self,
        method: String,
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<Vec<u8>>,
    ) -> HttpHostResponse {
        self.requests.lock().unwrap().push(RecordedRequest {
            method: method.clone(),
            url: url.clone(),
            headers,
            body,
        });
        let Some(route) = self.routes.iter().find(|r| url.contains(&r.needle)) else {
            // 宿主侧 `ProxyHttpError::Network(msg)` → code "network"、
            // error 为 Display 文本 "network: {msg}"，逐字对齐。
            return HttpHostResponse::failure(
                "network",
                format!("network: stub 代理无预设响应: {method} {url}"),
            );
        };
        HttpHostResponse::success(
            route.status,
            route.headers.clone(),
            BASE64.encode(route.body.as_bytes()),
        )
    }
}

// ---------------------------------------------------------------------------
// capability 桩：ABI 1.6 三条宿主函数的注入面（路由表 + 单值槽 + 请求记录）
// ---------------------------------------------------------------------------

/// `tma_catalog_read` 路由的匹配条件：`Some` 字段要求请求同名字段相等
/// （请求的 `Option` 字段即要求为 `Some(该值)`），`None` 不约束。
/// 注意匹配无法表达「请求字段必须为 None」——不设约束的字段对 None 与
/// 任意 Some 一律放行；要断言请求确实没带某字段，用
/// [`CapabilityStub::catalog_requests`] 记录的请求 DTO 判。
#[derive(Debug, Clone, Default)]
pub struct CatalogMatch {
    pub kind: Option<String>,
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl CatalogMatch {
    /// 只约束 `kind`（列表/详情查询最常用的区分维度）。
    pub fn kind(kind: &str) -> Self {
        Self {
            kind: Some(kind.into()),
            ..Default::default()
        }
    }

    /// 追加 `id` 等值约束（get-by-id 形态）。
    pub fn and_id(mut self, id: &str) -> Self {
        self.id = Some(id.into());
        self
    }

    /// 追加 `parent_id` 等值约束（children 列表形态）。
    pub fn and_parent_id(mut self, parent_id: &str) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// 追加 `query` 等值约束。
    pub fn and_query(mut self, query: &str) -> Self {
        self.query = Some(query.into());
        self
    }

    /// 追加 `cursor` 等值约束。
    pub fn and_cursor(mut self, cursor: &str) -> Self {
        self.cursor = Some(cursor.into());
        self
    }

    /// 追加 `limit` 等值约束。
    pub fn and_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    fn matches(&self, req: &CatalogReadRequest) -> bool {
        fn opt(want: &Option<String>, got: &Option<String>) -> bool {
            want.as_ref().is_none_or(|want| Some(want) == got.as_ref())
        }
        opt(&self.kind, &req.kind)
            && opt(&self.id, &req.id)
            && opt(&self.parent_id, &req.parent_id)
            && opt(&self.query, &req.query)
            && opt(&self.cursor, &req.cursor)
            && self.limit.is_none_or(|limit| limit == req.limit)
    }
}

/// `tma_media_stream` 路由的匹配条件（口径同 [`CatalogMatch`]）。
#[derive(Debug, Clone, Default)]
pub struct MediaMatch {
    pub media_id: Option<String>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub range: Option<MediaByteRange>,
}

impl MediaMatch {
    /// 只约束 `media_id`（最常用的区分维度）。
    pub fn media_id(media_id: &str) -> Self {
        Self {
            media_id: Some(media_id.into()),
            ..Default::default()
        }
    }

    /// 追加 `codec` 等值约束。
    pub fn and_codec(mut self, codec: &str) -> Self {
        self.codec = Some(codec.into());
        self
    }

    /// 追加 `bitrate_kbps` 等值约束。
    pub fn and_bitrate_kbps(mut self, bitrate_kbps: u32) -> Self {
        self.bitrate_kbps = Some(bitrate_kbps);
        self
    }

    /// 追加 `range` 等值约束（要求请求带 `Some(range)`）。
    pub fn and_range(mut self, start: u64, end: Option<u64>) -> Self {
        self.range = Some(MediaByteRange { start, end });
        self
    }

    fn matches(&self, req: &MediaStreamRequest) -> bool {
        self.media_id.as_deref().is_none_or(|id| id == req.media_id)
            && self
                .codec
                .as_deref()
                .is_none_or(|c| req.codec.as_deref() == Some(c))
            && self
                .bitrate_kbps
                .is_none_or(|b| req.bitrate_kbps == Some(b))
            && self.range.is_none_or(|r| req.range == Some(r))
    }
}

/// catalog 成功响应（`version` 自动填 [`CAPABILITY_DTO_VERSION`]，无 next_cursor）。
pub fn catalog_ok(items: Vec<CatalogItem>) -> CatalogReadResponse {
    CatalogReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        items,
        next_cursor: None,
        error: None,
    }
}

/// catalog 业务失败（`ok:false` DTO——与「桩未命中」的宿主级 `Err`
/// 是两种失败形态，测试别混用）。
pub fn catalog_err(code: &str, message: &str) -> CatalogReadResponse {
    CatalogReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        items: Vec::new(),
        next_cursor: None,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// `CatalogItem` 便捷构造：必备三字段 + 其余全 `None`，测试按需覆盖字段。
pub fn catalog_item(id: &str, kind: &str, title: &str) -> CatalogItem {
    CatalogItem {
        id: id.into(),
        kind: kind.into(),
        title: title.into(),
        parent_id: None,
        album_id: None,
        duration_ms: None,
        artist: None,
        album: None,
        track_number: None,
        disc_number: None,
        year: None,
        genre: None,
        format: None,
        bitrate: None,
        size: None,
        cover_art_id: None,
    }
}

/// 已认证身份快照（`is_admin` 按宿主语义注入）。
pub fn identity_ok(user_id: &str, username: &str, is_admin: bool) -> IdentityReadResponse {
    IdentityReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        user_id: Some(user_id.into()),
        username: Some(username.into()),
        is_admin,
        error: None,
    }
}

/// 未认证/身份读取业务失败（`ok:false` DTO）。
pub fn identity_err(code: &str, message: &str) -> IdentityReadResponse {
    IdentityReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        user_id: None,
        username: None,
        is_admin: false,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// media 成功响应最小构造；`content_length`/`content_range`/`stream_id`
/// 需要时在返回结构上直接改字段。
pub fn media_ok(status: u16, content_type: &str, body_b64: Option<String>) -> MediaStreamResponse {
    MediaStreamResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        status,
        content_type: Some(content_type.into()),
        content_length: None,
        content_range: None,
        body_b64,
        stream_id: None,
        error: None,
    }
}

/// media 业务失败（`status` 如 404/416 由插件自行翻译成协议错误）。
pub fn media_err(status: u16, code: &str, message: &str) -> MediaStreamResponse {
    MediaStreamResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        status,
        content_type: None,
        content_length: None,
        content_range: None,
        body_b64: None,
        stream_id: None,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// 三条 capability 宿主函数的注入面：catalog/media 各一张「匹配条件 → 响应」
/// 路由表（先注册先命中），identity 单值槽；进入派发的每次调用都先记录
/// 完整请求 DTO（在宿主函数入口校验/权限门阶段被拒绝的调用不进入桩派发、
/// 不留痕——与宿主把校验放在 provider 之前一致）。
///
/// 未配置（identity 槽空 / 路由表为空）或未命中的调用返回宿主函数级 `Err`
/// （整个导出调用当场失败，错误消息带 capability 名与请求摘要）——与
/// [`StubProxy`] 「先记录再明确失败」同精神，但刻意不用 `ok:false` DTO，
/// 避免把桩缺配置误读成已配置的业务失败。
#[derive(Default)]
pub struct CapabilityStub {
    identity: Mutex<Option<IdentityReadResponse>>,
    catalog: Mutex<Vec<(CatalogMatch, CatalogReadResponse)>>,
    media: Mutex<Vec<(MediaMatch, MediaStreamResponse)>>,
    identity_requests: Mutex<Vec<IdentityReadRequest>>,
    catalog_requests: Mutex<Vec<CatalogReadRequest>>,
    media_requests: Mutex<Vec<MediaStreamRequest>>,
}

impl CapabilityStub {
    /// 全未配置的空桩：进入桩派发的任何调用都先记录后 `Err`（入口校验或
    /// 权限门拒绝的调用不进入派发——宿主函数直接写回结构化错误 DTO）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 已认证身份 + 空路由表（入站 HTTP 插件的最常用入口）。
    pub fn authenticated(user_id: &str, username: &str, is_admin: bool) -> Arc<Self> {
        Self::new().with_identity(identity_ok(user_id, username, is_admin))
    }

    /// 写入 identity 槽位（`ok:false` 等形态也走这里）；返回同一 Arc 供链式。
    pub fn with_identity(self: Arc<Self>, response: IdentityReadResponse) -> Arc<Self> {
        *self.identity.lock().unwrap() = Some(response);
        self
    }

    /// 追加一条 catalog 路由（先注册先命中）；返回同一 Arc 供链式。
    pub fn with_catalog(
        self: Arc<Self>,
        matcher: CatalogMatch,
        response: CatalogReadResponse,
    ) -> Arc<Self> {
        self.catalog.lock().unwrap().push((matcher, response));
        self
    }

    /// 追加一条 media 路由（先注册先命中）；返回同一 Arc 供链式。
    pub fn with_media(
        self: Arc<Self>,
        matcher: MediaMatch,
        response: MediaStreamResponse,
    ) -> Arc<Self> {
        self.media.lock().unwrap().push((matcher, response));
        self
    }

    /// 已记录的 `tma_catalog_read` 入参（含未命中调用）。
    pub fn catalog_requests(&self) -> Vec<CatalogReadRequest> {
        self.catalog_requests.lock().unwrap().clone()
    }

    /// 已记录的 `tma_identity_read` 入参（含未配置调用）。
    pub fn identity_requests(&self) -> Vec<IdentityReadRequest> {
        self.identity_requests.lock().unwrap().clone()
    }

    /// 已记录的 `tma_media_stream` 入参（含未命中调用）。
    pub fn media_requests(&self) -> Vec<MediaStreamRequest> {
        self.media_requests.lock().unwrap().clone()
    }

    /// `tma_catalog_read` 宿主函数的业务段：先记录再按路由表匹配。
    fn dispatch_catalog(
        &self,
        req: CatalogReadRequest,
    ) -> Result<CatalogReadResponse, extism::Error> {
        self.catalog_requests.lock().unwrap().push(req.clone());
        self.catalog
            .lock()
            .unwrap()
            .iter()
            .find(|(matcher, _)| matcher.matches(&req))
            .map(|(_, response)| response.clone())
            .ok_or_else(|| extism::Error::msg(format!("tma_catalog_read 桩无预设响应: {req:?}")))
    }

    /// `tma_identity_read` 宿主函数的业务段：先记录再取单值槽。
    fn dispatch_identity(
        &self,
        req: IdentityReadRequest,
    ) -> Result<IdentityReadResponse, extism::Error> {
        self.identity_requests.lock().unwrap().push(req.clone());
        self.identity
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| extism::Error::msg(format!("tma_identity_read 桩无预设响应: {req:?}")))
    }

    /// `tma_media_stream` 宿主函数的业务段：先记录再按路由表匹配。
    fn dispatch_media(
        &self,
        req: MediaStreamRequest,
    ) -> Result<MediaStreamResponse, extism::Error> {
        self.media_requests.lock().unwrap().push(req.clone());
        self.media
            .lock()
            .unwrap()
            .iter()
            .find(|(matcher, _)| matcher.matches(&req))
            .map(|(_, response)| response.clone())
            .ok_or_else(|| extism::Error::msg(format!("tma_media_stream 桩无预设响应: {req:?}")))
    }
}

// ---------------------------------------------------------------------------
// 宿主函数桩（注册到 extism 默认 `extism:host/user` 命名空间）
// ---------------------------------------------------------------------------

/// 宿主函数 user data：stub 代理 + capability 桩 + 注入的运行时配置 JSON +
/// 清单声明的 capability 授权集合。
#[derive(Clone)]
struct HostFnState {
    proxy: Arc<StubProxy>,
    capabilities: Arc<CapabilityStub>,
    runtime_config: Arc<str>,
    /// `Some` = 验签包路径，只放行 manifest `permissions` 声明过的
    /// capability；`None` = 无清单路径（`from_wasm*`），全部放行。
    granted: Option<Arc<BTreeSet<String>>>,
}

/// capability 权限门前的判定（对应宿主 `gate::capability_allowed`）：
/// `granted` 为 `Some` 时放行清单声明过的 capability 名；`None`（裸 wasm
/// 路径）全放行。
fn capability_denied(state: &HostFnState, capability: &str) -> bool {
    state
        .granted
        .as_ref()
        .is_some_and(|granted| !granted.contains(capability))
}

/// capability 应答 DTO 的统一构造面：宿主对入参损坏、DTO 版本不符、权限
/// 门拒绝、provider 未注入/panic 等前置失败一律**写回结构化错误应答**
/// （`ok:false` + `CapabilityError`），不 trap 导出调用。`status` 仅
/// `MediaStreamResponse` 使用（与宿主 `media_error` 同口径），
/// catalog/identity 应答忽略该参数。
trait CapabilityReply: serde::Serialize {
    fn error_response(version: u16, status: u16, code: &str, message: String) -> Self;
}

impl CapabilityReply for CatalogReadResponse {
    fn error_response(version: u16, _status: u16, code: &str, message: String) -> Self {
        Self {
            version,
            ok: false,
            items: vec![],
            next_cursor: None,
            error: Some(CapabilityError::new(code, message)),
        }
    }
}

impl CapabilityReply for IdentityReadResponse {
    fn error_response(version: u16, _status: u16, code: &str, message: String) -> Self {
        Self {
            version,
            ok: false,
            user_id: None,
            username: None,
            is_admin: false,
            error: Some(CapabilityError::new(code, message)),
        }
    }
}

impl CapabilityReply for MediaStreamResponse {
    fn error_response(version: u16, status: u16, code: &str, message: String) -> Self {
        Self {
            version,
            ok: false,
            status,
            content_type: None,
            content_length: None,
            content_range: None,
            body_b64: None,
            stream_id: None,
            error: Some(CapabilityError::new(code, message)),
        }
    }
}

/// 从清单提取声明过的 capability 名集合（`permissions[].capability.name`），
/// 作为验签包路径逐次权限门的放行集；`http`/`ai` 权限不映射到 capability
/// 宿主函数。
fn granted_capabilities(manifest: &PluginManifest) -> BTreeSet<String> {
    manifest
        .permissions
        .iter()
        .filter_map(|permission| match permission {
            tma_plugin_sdk::Permission::Capability { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// `http_request`（I64 offset 入 → I64 offset 出）：入参/出参映射与宿主
/// `http_request_host_fn` 逐字段一致；业务段把「带权限门与重定向的异步代理」
/// 替换为同步 [`StubProxy::dispatch`]，其余（offset 读取/释放、JSON 与
/// body_b64 编解码、panic 兜底为 `ok:false`）原样复现。
fn http_request_host_fn(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    let out = std::panic::catch_unwind(AssertUnwindSafe(|| -> HttpHostResponse {
        let state = match user_data.get().and_then(|cell| {
            cell.lock()
                .map_err(|_| extism::Error::msg("user data 锁中毒"))
                .map(|guard| guard.clone())
        }) {
            Ok(s) => s,
            Err(e) => {
                return HttpHostResponse::failure("internal", format!("user data 不可用: {e}"));
            }
        };
        let offset = match input.first().and_then(|v| v.i64()) {
            Some(o) if o > 0 => o as u64,
            _ => {
                return HttpHostResponse::failure(
                    "bad_request",
                    "http_request 需要 i64 入参 offset",
                );
            }
        };
        let handle = match plugin.memory_handle(offset) {
            Some(h) => h,
            None => {
                return HttpHostResponse::failure(
                    "bad_request",
                    format!("非法入参 offset: {offset}"),
                );
            }
        };
        let bytes = match plugin.memory_bytes(handle) {
            Ok(b) => b.to_vec(),
            Err(e) => {
                return HttpHostResponse::failure("bad_request", format!("读取入参失败: {e}"));
            }
        };
        if let Err(e) = plugin.memory_free(handle) {
            return HttpHostResponse::failure("internal", format!("释放入参失败: {e}"));
        }

        let call: HttpHostRequest = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(e) => {
                return HttpHostResponse::failure("bad_request", format!("入参 JSON 非法: {e}"));
            }
        };

        let body = match call.body_b64.as_deref() {
            None | Some("") => None,
            Some(b64) => match BASE64.decode(b64) {
                Ok(b) => Some(b),
                Err(e) => {
                    return HttpHostResponse::failure("bad_request", format!("body_b64 非法: {e}"));
                }
            },
        };

        state
            .proxy
            .dispatch(call.method, call.url, call.headers, body)
    }));

    let out = match out {
        Ok(v) => v,
        Err(_) => HttpHostResponse::failure("internal", "http_request 宿主函数 panic（已兜底）"),
    };
    let bytes = serde_json::to_vec(&out)
        .map_err(|e| extism::Error::msg(format!("http_request 响应序列化失败: {e}")))?;
    let mem = plugin
        .memory_new(bytes)
        .map_err(|e| e.context("http_request 写回内存失败"))?;
    if let Some(out) = output.first_mut() {
        *out = Val::I64(mem.offset() as i64);
    }
    Ok(())
}

/// `tma_config`（无参 → I64 offset）：返回注入的运行时配置 JSON 字符串
/// （与宿主 `tma_config_host_fn` 同语义；插件侧 `load_config` 对 `{}`/空串兜底）。
fn tma_config_host_fn(
    plugin: &mut CurrentPlugin,
    _input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    let state = user_data
        .get()
        .and_then(|cell| {
            cell.lock()
                .map_err(|_| extism::Error::msg("user data 锁中毒"))
                .map(|guard| guard.clone())
        })
        .map_err(|e| extism::Error::msg(format!("tma_config: user data 不可用: {e}")))?;
    let bytes = state.runtime_config.as_bytes().to_vec();
    let mem = plugin
        .memory_new(bytes)
        .map_err(|e| e.context("tma_config 写回内存失败"))?;
    if let Some(out) = output.first_mut() {
        *out = Val::I64(mem.offset() as i64);
    }
    Ok(())
}

/// capability 宿主函数的公共协议段（`extism_pdk::Json` 包装 = i64 offset 进、
/// i64 offset 出，与 `http_request` 同一跨边界协议），与宿主
/// `capability_fns.rs` 同骨架：i64 offset → UTF-8 JSON 入参 → `respond` →
/// JSON 响应 → 写回新 offset。
///
/// 与宿主同口径：入参读取/解析失败不 trap，而是把 `Err(message)` 交给
/// `respond`，由各应答构造器写成 `bad_request` DTO；状态不可用写回
/// `internal` DTO。与宿主的刻意差异只剩两处测试边界：桩未配置/未命中的
/// 请求仍走宿主级 `Err`（大声暴露夹具缺配置，而非宿主 `unavailable`
/// DTO——testkit 的桩即 provider，始终存在），桩 panic 同样兜底为 `Err`
/// （宿主对应 `internal` DTO）。
fn capability_json_call<Req, Resp>(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
    name: &'static str,
    respond: impl FnOnce(&HostFnState, Result<Req, String>) -> Result<Resp, extism::Error>,
) -> Result<(), extism::Error>
where
    Req: serde::de::DeserializeOwned,
    Resp: CapabilityReply,
{
    // 与宿主同口径：宿主状态不可用时写回 internal DTO，不 trap。
    let state = user_data
        .get()
        .ok()
        .and_then(|cell| cell.lock().map(|guard| guard.clone()).ok());
    let Some(state) = state else {
        return write_capability_output(
            plugin,
            output,
            &Resp::error_response(
                CAPABILITY_DTO_VERSION,
                500,
                "internal",
                "capability 状态不可用".into(),
            ),
            name,
        );
    };
    let parsed: Result<Req, String> = (|| {
        let offset = input
            .first()
            .and_then(|v| v.i64())
            .filter(|offset| *offset > 0)
            .ok_or_else(|| format!("{name} 需要 i64 入参 offset"))? as u64;
        let handle = plugin
            .memory_handle(offset)
            .ok_or_else(|| format!("非法入参 offset: {offset}"))?;
        let bytes = plugin
            .memory_bytes(handle)
            .map_err(|e| format!("读取入参失败: {e}"))?
            .to_vec();
        plugin
            .memory_free(handle)
            .map_err(|e| format!("释放入参失败: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("入参 JSON 非法: {e}"))
    })();

    // 桩 panic 兜底为宿主级 Err（宿主对应 provider panic → internal DTO；
    // 桩 panic 意味着夹具自身坏了，大声失败更利于定位）。
    let response = std::panic::catch_unwind(AssertUnwindSafe(|| respond(&state, parsed)))
        .map_err(|_| extism::Error::msg(format!("{name} 桩 panic（已兜底）")))??;
    write_capability_output(plugin, output, &response, name)
}

/// 写回 capability 应答 DTO（对应宿主 `write_capability_output`）。
fn write_capability_output<Resp: serde::Serialize>(
    plugin: &mut CurrentPlugin,
    output: &mut [Val],
    response: &Resp,
    name: &'static str,
) -> Result<(), extism::Error> {
    let bytes = serde_json::to_vec(response)
        .map_err(|e| extism::Error::msg(format!("{name} 响应序列化失败: {e}")))?;
    let mem = plugin
        .memory_new(bytes)
        .map_err(|e| e.context(format!("{name} 写回内存失败")))?;
    if let Some(out) = output.first_mut() {
        *out = Val::I64(mem.offset() as i64);
    }
    Ok(())
}

/// `tma_catalog_read`（I64 → I64）：ABI 1.6 曲库只读查询，注入面是
/// [`CapabilityStub`] 的 catalog 路由表。校验顺序与宿主一致：
/// 入参 → DTO 版本 → `catalog.read` 权限门 → `limit` 范围 → 桩派发。
fn tma_catalog_read_host_fn(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    capability_json_call(
        plugin,
        input,
        output,
        user_data,
        "tma_catalog_read",
        catalog_respond,
    )
}

/// `tma_identity_read`（I64 → I64）：ABI 1.6 调用身份快照，注入面是
/// [`CapabilityStub`] 的 identity 单值槽。校验顺序与宿主一致：
/// 入参 → DTO 版本 → `identity.read` 权限门 → 桩派发。
fn tma_identity_read_host_fn(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    capability_json_call(
        plugin,
        input,
        output,
        user_data,
        "tma_identity_read",
        identity_respond,
    )
}

/// `tma_media_stream`（I64 → I64）：ABI 1.6 按 media id 读媒体流，注入面是
/// [`CapabilityStub`] 的 media 路由表。校验顺序与宿主一致：入参 → DTO
/// 版本 → `media_id` 非空 → range 合法 → `media.stream` 权限门 → 桩派发。
fn tma_media_stream_host_fn(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    capability_json_call(
        plugin,
        input,
        output,
        user_data,
        "tma_media_stream",
        media_respond,
    )
}

/// `tma_catalog_read` 的业务段（与宿主 `catalog_read_host_fn` 同序）：
/// 入参失败 → `bad_request`；DTO 版本不符 → `unsupported_version`；
/// `catalog.read` 未声明 → `forbidden`；`limit` 越界 → `bad_request`；
/// 否则进桩派发（桩缺配置/未命中 `Err` 上抛为宿主级失败）。
fn catalog_respond(
    state: &HostFnState,
    parsed: Result<CatalogReadRequest, String>,
) -> Result<CatalogReadResponse, extism::Error> {
    Ok(match parsed {
        Err(message) => {
            CatalogReadResponse::error_response(CAPABILITY_DTO_VERSION, 0, "bad_request", message)
        }
        Ok(request) if request.version != CAPABILITY_DTO_VERSION => {
            CatalogReadResponse::error_response(
                CAPABILITY_DTO_VERSION,
                0,
                "unsupported_version",
                "不支持的 catalog DTO 版本".into(),
            )
        }
        Ok(request) if capability_denied(state, "catalog.read") => {
            CatalogReadResponse::error_response(
                request.version,
                0,
                "forbidden",
                "插件未声明 catalog.read 权限".into(),
            )
        }
        Ok(request) if request.limit == 0 || request.limit > 1_000 => {
            CatalogReadResponse::error_response(
                request.version,
                0,
                "bad_request",
                "limit 必须在 1..=1000 内".into(),
            )
        }
        Ok(request) => state.capabilities.dispatch_catalog(request)?,
    })
}

/// `tma_identity_read` 的业务段（与宿主 `identity_read_host_fn` 同序）：
/// 入参失败 → `bad_request`；DTO 版本不符 → `unsupported_version`；
/// `identity.read` 未声明 → `forbidden`；否则进桩派发。
fn identity_respond(
    state: &HostFnState,
    parsed: Result<IdentityReadRequest, String>,
) -> Result<IdentityReadResponse, extism::Error> {
    Ok(match parsed {
        Err(message) => {
            IdentityReadResponse::error_response(CAPABILITY_DTO_VERSION, 0, "bad_request", message)
        }
        Ok(request) if request.version != CAPABILITY_DTO_VERSION => {
            IdentityReadResponse::error_response(
                CAPABILITY_DTO_VERSION,
                0,
                "unsupported_version",
                "不支持的 identity DTO 版本".into(),
            )
        }
        Ok(request) if capability_denied(state, "identity.read") => {
            IdentityReadResponse::error_response(
                request.version,
                0,
                "forbidden",
                "插件未声明 identity.read 权限".into(),
            )
        }
        Ok(request) => state.capabilities.dispatch_identity(request)?,
    })
}

/// `tma_media_stream` 的业务段（与宿主 `media_stream_host_fn` 同序）：
/// 入参失败 → 400 `bad_request`；DTO 版本不符 → 400 `unsupported_version`；
/// `media_id` 空 → 400 `bad_request`；range end < start → 416
/// `invalid_range`；`media.stream` 未声明 → 403 `forbidden`；否则进桩派发。
fn media_respond(
    state: &HostFnState,
    parsed: Result<MediaStreamRequest, String>,
) -> Result<MediaStreamResponse, extism::Error> {
    Ok(match parsed {
        Err(message) => {
            MediaStreamResponse::error_response(CAPABILITY_DTO_VERSION, 400, "bad_request", message)
        }
        Ok(request) if request.version != CAPABILITY_DTO_VERSION => {
            MediaStreamResponse::error_response(
                CAPABILITY_DTO_VERSION,
                400,
                "unsupported_version",
                "不支持的 media DTO 版本".into(),
            )
        }
        Ok(request) if request.media_id.trim().is_empty() => MediaStreamResponse::error_response(
            request.version,
            400,
            "bad_request",
            "media_id 不能为空".into(),
        ),
        Ok(request)
            if request
                .range
                .is_some_and(|range| range.end.is_some_and(|end| end < range.start)) =>
        {
            MediaStreamResponse::error_response(
                request.version,
                416,
                "invalid_range",
                "range end 不能小于 start".into(),
            )
        }
        Ok(request) if capability_denied(state, "media.stream") => {
            MediaStreamResponse::error_response(
                request.version,
                403,
                "forbidden",
                "插件未声明 media.stream 权限".into(),
            )
        }
        Ok(request) => state.capabilities.dispatch_media(request)?,
    })
}

// ---------------------------------------------------------------------------
// 测试签名密钥对（本仓验收测试专用，勿作任何真实分发签名/受信公钥）
// ---------------------------------------------------------------------------

/// 测试专用 ed25519 私钥（base64 32 字节种子，`tma-plugin-dev keygen` 现生成后
/// 冻结在此）。仅用于 `pack_dir`/`verify` 的自洽链路——不是生产签名私钥，
/// 也不应进入任何宿主的受信公钥集合。
pub const TEST_SIGNING_KEY_B64: &str = "f3ctCS9uN2C3TqFwsZ69XgN2laeePKZpzhZqJLGjIns=";

/// 测试专用签名私钥（从 [`TEST_SIGNING_KEY_B64`] 解码）。
pub fn test_signing_key() -> SigningKey {
    let bytes: [u8; 32] = BASE64
        .decode(TEST_SIGNING_KEY_B64)
        .expect("测试私钥必须是合法 base64")
        .try_into()
        .expect("测试私钥必须是 32 字节 ed25519 种子");
    SigningKey::from_bytes(&bytes)
}

/// 与测试私钥配对的公钥（`verify` 的受信集合）。
pub fn test_verifying_key() -> VerifyingKey {
    test_signing_key().verifying_key()
}

// ---------------------------------------------------------------------------
// 打包 / 验签 / 加载
// ---------------------------------------------------------------------------

/// 确保插件 wasm 产物存在：`cargo build --target wasm32-unknown-unknown
/// --release`（cargo 增量缓存，重复调用近乎无成本）。返回产物路径。
pub fn ensure_wasm_built(plugin_dir: &Path) -> PathBuf {
    let toml = std::fs::read_to_string(plugin_dir.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("读 {:?}/Cargo.toml 失败: {e}", plugin_dir));
    // wasm 产物名 = crate 名连字符转下划线；crate 名取 Cargo.toml 首个
    // `name = "..."`（与 scripts/build-package.sh 同口径）。
    let crate_name = toml
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or_else(|| panic!("无法从 {:?}/Cargo.toml 解析 package name", plugin_dir))
        .to_string();
    let wasm = plugin_dir.join(format!(
        "target/wasm32-unknown-unknown/release/{}.wasm",
        crate_name.replace('-', "_")
    ));
    let status = std::process::Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(plugin_dir)
        .status()
        .expect("启动 cargo build 失败（需要 wasm32-unknown-unknown target）");
    assert!(
        status.success(),
        "cargo build --target wasm32-unknown-unknown --release 失败（{plugin_dir:?}）"
    );
    assert!(wasm.is_file(), "缺少 {wasm:?}（crate {crate_name}）");
    wasm
}

/// 读可选图标条目（`icon.svg` 优先、`icon.png` 其次，至多一个——与包格式一致）。
fn read_icon(plugin_dir: &Path) -> Option<PluginIcon> {
    for name in ["icon.svg", "icon.png"] {
        if let Ok(bytes) = std::fs::read(plugin_dir.join(name)) {
            return Some(PluginIcon { name, bytes });
        }
    }
    None
}

/// 把插件目录打成真实签名的 `.tmap`：构建 wasm → `manifest.json` 原始字节 +
/// wasm + 可选图标 → 测试私钥 `pack`。
pub fn pack_dir(plugin_dir: &Path) -> Result<Vec<u8>, tma_plugin_sdk::PackageError> {
    let wasm_path = ensure_wasm_built(plugin_dir);
    let manifest = std::fs::read(plugin_dir.join("manifest.json"))
        .unwrap_or_else(|e| panic!("读 {:?}/manifest.json 失败: {e}", plugin_dir));
    let wasm = std::fs::read(&wasm_path).unwrap_or_else(|e| panic!("读 {wasm_path:?} 失败: {e}"));
    let key = test_signing_key();
    match read_icon(plugin_dir) {
        Some(icon) => tma_plugin_sdk::pack_with_icon(&manifest, &wasm, &icon, &key),
        None => tma_plugin_sdk::pack(&manifest, &wasm, &key),
    }
}

/// 验签 `.tmap` 字节（受信集合 = [`test_verifying_key`]）：解包 → 验签 →
/// 清单校验 → ABI 兼容，与宿主安装入口同源。
pub fn verify(tmap_bytes: &[u8]) -> Result<VerifiedPlugin, tma_plugin_sdk::PackageError> {
    tma_plugin_sdk::verify(tmap_bytes, &[test_verifying_key()])
}

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn internal_error(message: String) -> PluginError {
    PluginError::new(PluginErrorCode::Internal, message)
}

/// 完整 source chain（extism 错误常以 context 包装底层 trap，只看 to_string
/// 会丢掉真正原因）。
fn error_chain(e: &extism::Error) -> String {
    let mut out = e.to_string();
    for cause in e.chain().skip(1) {
        out.push_str(": ");
        out.push_str(&cause.to_string());
    }
    out
}

/// 已编译并绑定宿主函数的插件：并发安全，每次调用 instance-per-call 重实例化。
pub struct LoadedPlugin {
    compiled: extism::CompiledPlugin,
}

impl LoadedPlugin {
    /// 直接从未验签 wasm 字节构造（`load_verified` 之下；清单无关路径可用）。
    ///
    /// `runtime_config` 为注入的运行时配置 JSON（未配置传 `"{}"`），实例化后
    /// 经 `tma_config` 宿主函数对插件可见。capability 桩取全未配置空桩——
    /// 宿主函数无条件注册，老插件不 import 即无副作用；裸 wasm 路径无清单
    /// 可据，capability 权限门全放行（验签包路径见
    /// [`Self::from_verified_with_capabilities`]）。
    pub fn from_wasm(
        wasm: &[u8],
        runtime_config: &str,
        proxy: Arc<StubProxy>,
    ) -> Result<Self, PluginError> {
        Self::from_wasm_with_capabilities(wasm, runtime_config, proxy, CapabilityStub::new())
    }

    /// `from_wasm` 的 capability 变体：额外注入 [`CapabilityStub`] 应答
    /// `tma_catalog_read`/`tma_identity_read`/`tma_media_stream`。
    ///
    /// 裸 wasm 路径不带清单，capability 权限门全放行；要复现「清单未声明
    /// 的 capability 被宿主拒」请走验签包路径（[`Self::from_verified_with_capabilities`]）。
    pub fn from_wasm_with_capabilities(
        wasm: &[u8],
        runtime_config: &str,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
    ) -> Result<Self, PluginError> {
        Self::from_wasm_inner(wasm, runtime_config, proxy, capabilities, None)
    }

    fn from_wasm_inner(
        wasm: &[u8],
        runtime_config: &str,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
        granted: Option<Arc<BTreeSet<String>>>,
    ) -> Result<Self, PluginError> {
        let state = HostFnState {
            proxy,
            capabilities,
            runtime_config: Arc::from(runtime_config),
            granted,
        };
        let functions = vec![
            Function::new(
                "http_request",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                http_request_host_fn,
            ),
            Function::new(
                "tma_config",
                [],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_config_host_fn,
            ),
            Function::new(
                "tma_catalog_read",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_catalog_read_host_fn,
            ),
            Function::new(
                "tma_identity_read",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_identity_read_host_fn,
            ),
            Function::new(
                "tma_media_stream",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state),
                tma_media_stream_host_fn,
            ),
        ];
        // 与原验收夹具的 WasmEngineConfig 一致：10s 超时、无 fuel、1024 页上限。
        let manifest = extism::Manifest::new([extism::Wasm::data(wasm.to_vec())])
            .with_timeout(Duration::from_secs(10))
            .with_memory_max(1024);
        let compiled = extism::PluginBuilder::new(manifest)
            .with_functions(functions)
            .compile()
            .map_err(|e| internal_error(format!("wasm 编译失败: {}", error_chain(&e))))?;
        Ok(Self { compiled })
    }

    /// 验签包 → 实例化 → `tma_manifest` 探测比对（存在导出即须与包内清单一致，
    /// 与宿主 `load_verified` 同语义）。capability 桩取全未配置空桩；
    /// 逐次 capability 权限门按清单 `permissions` 生效（见
    /// [`Self::from_verified_with_capabilities`]）。
    pub fn from_verified(
        verified: &VerifiedPlugin,
        runtime_config: &str,
        proxy: Arc<StubProxy>,
    ) -> Result<Self, PluginError> {
        Self::from_verified_with_capabilities(
            verified,
            runtime_config,
            proxy,
            CapabilityStub::new(),
        )
    }

    /// `from_verified` 的 capability 变体：除 [`Self::from_wasm_with_capabilities`]
    /// 的行为外，还按 `verified.manifest` 的 `permissions` 复现逐次
    /// capability 权限门——插件调用清单未声明的 capability 时宿主函数写回
    /// `ok:false` + `forbidden` 应答（media 为 403），与真实宿主同口径。
    pub fn from_verified_with_capabilities(
        verified: &VerifiedPlugin,
        runtime_config: &str,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
    ) -> Result<Self, PluginError> {
        let plugin = Self::from_wasm_inner(
            &verified.wasm,
            runtime_config,
            proxy,
            capabilities,
            Some(Arc::new(granted_capabilities(&verified.manifest))),
        )?;
        if let Some(exported) = plugin.probe_manifest()?
            && verified.manifest != exported
        {
            return Err(internal_error(
                "tma_manifest 导出与包内清单不一致，拒绝安装".into(),
            ));
        }
        Ok(plugin)
    }

    /// `tma_manifest` 导出探测（`Ok(None)` = 无该导出）。
    fn probe_manifest(&self) -> Result<Option<PluginManifest>, PluginError> {
        let mut plugin = self.instantiate()?;
        if !plugin.function_exists("tma_manifest") {
            return Ok(None);
        }
        let bytes = plugin
            .call::<&[u8], Vec<u8>>("tma_manifest", b"")
            .map_err(|e| internal_error(format!("tma_manifest 调用失败: {}", error_chain(&e))))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| internal_error(format!("tma_manifest 返回非法清单 JSON: {e}")))
    }

    fn instantiate(&self) -> Result<extism::Plugin, PluginError> {
        extism::Plugin::new_from_compiled(&self.compiled)
            .map_err(|e| internal_error(format!("wasm 实例化失败: {}", error_chain(&e))))
    }

    fn call_export(&self, export: &str, input: &[u8]) -> Result<Vec<u8>, PluginError> {
        let mut plugin = self.instantiate()?;
        plugin
            .call::<&[u8], Vec<u8>>(export, input)
            .map_err(|e| internal_error(format!("{export} 调用失败: {}", error_chain(&e))))
    }

    /// `scrape` 导出的信封调用（`PluginRequest`/`PluginResponse` 往返，
    /// 响应 id 必须原样带回——与宿主 `invoke` 同语义）。
    fn invoke(&self, op: PluginOp) -> Result<PluginOutcome, PluginError> {
        let req = PluginRequest {
            id: next_request_id(),
            op,
        };
        let input = serde_json::to_vec(&req)
            .map_err(|e| internal_error(format!("请求信封序列化失败: {e}")))?;
        let bytes = self.call_export("scrape", &input)?;
        let resp = serde_json::from_slice::<PluginResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法响应 JSON: {e}")))?;
        if resp.id != req.id {
            return Err(internal_error(format!(
                "响应信封 id 不匹配（期望 {}，实际 {}）",
                req.id, resp.id
            )));
        }
        match resp.outcome {
            PluginOutcome::Error(e) => Err(e),
            other => Ok(other),
        }
    }

    /// 单实体刮削（`scrape` 导出，`PluginOp::Scrape`）。
    pub fn call_scrape(&self, query: EntityQuery) -> Result<ScrapeResult, PluginError> {
        match self.invoke(PluginOp::Scrape { query })? {
            PluginOutcome::Scrape(result) => Ok(*result),
            PluginOutcome::Discography(_) => {
                Err(internal_error("scrape 请求收到 discography 响应".into()))
            }
            PluginOutcome::Error(e) => Err(e),
        }
    }

    /// `tma_action` 导出（无导出 → `Unsupported`，与宿主同语义）。
    pub fn call_action(
        &self,
        request: PluginActionRequest,
    ) -> Result<PluginActionResponse, PluginError> {
        if !self.export_exists("tma_action")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_action，不支持动作".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("动作请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_action", &input)?;
        serde_json::from_slice::<PluginActionResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法动作响应 JSON: {e}")))
    }

    /// `tma_event` 导出（无导出 → `Unsupported`，与宿主同语义）。
    pub fn call_event(
        &self,
        request: PluginEventRequest,
    ) -> Result<PluginEventResponse, PluginError> {
        if !self.export_exists("tma_event")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_event，不支持事件订阅".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("事件请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_event", &input)?;
        serde_json::from_slice::<PluginEventResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法事件响应 JSON: {e}")))
    }

    /// `tma_http` 导出（ABI 1.6 入站 HTTP 扩展点；无导出 → `Unsupported`，
    /// 与 `call_action`/`call_event` 同口径）。业务 `Err` 被插件折叠为非零
    /// 退出码，经 `call_export` 落为 Internal + 完整错误链。
    pub fn call_http(&self, request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
        if !self.export_exists("tma_http")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_http，不支持入站 HTTP".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("HTTP 请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_http", &input)?;
        serde_json::from_slice::<PluginHttpResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法 HTTP 响应 JSON: {e}")))
    }

    fn export_exists(&self, name: &str) -> Result<bool, PluginError> {
        let plugin = self.instantiate()?;
        Ok(plugin.function_exists(name))
    }
}

/// 一条命令到底的加载：构建 wasm → pack → verify → 实例化（含 manifest 探测）。
/// 任一步失败带上下文 panic——夹具层的失败本来就该让测试红掉。
/// capability 桩取全未配置空桩（三条宿主函数照常注册）。
pub fn load_dir(plugin_dir: &Path, runtime_config: &str, proxy: Arc<StubProxy>) -> LoadedPlugin {
    load_dir_with_capabilities(plugin_dir, runtime_config, proxy, CapabilityStub::new())
}

/// `load_dir` 的 capability 变体：额外注入 [`CapabilityStub`] 应答
/// `tma_catalog_read`/`tma_identity_read`/`tma_media_stream`，驱动 ABI 1.6
/// 插件（如 subsonic）的 `tma_http` 全链路验收。
pub fn load_dir_with_capabilities(
    plugin_dir: &Path,
    runtime_config: &str,
    proxy: Arc<StubProxy>,
    capabilities: Arc<CapabilityStub>,
) -> LoadedPlugin {
    let tmap = pack_dir(plugin_dir).unwrap_or_else(|e| panic!("{plugin_dir:?} 打包失败: {e}"));
    let verified = verify(&tmap).unwrap_or_else(|e| panic!("{plugin_dir:?} 验签失败: {e}"));
    LoadedPlugin::from_verified_with_capabilities(&verified, runtime_config, proxy, capabilities)
        .unwrap_or_else(|e| panic!("{plugin_dir:?} 实例化失败: {e:?}"))
}

// ---------------------------------------------------------------------------
// 查询构造夹具（与原 `builtin_tests::support` 同名同形状）
// ---------------------------------------------------------------------------

pub fn artist_query(mbid: Option<&str>, name: Option<&str>) -> EntityQuery {
    EntityQuery {
        kind: ScrapeEntityKind::Artist,
        mbid: mbid.map(str::to_string),
        isrc: None,
        name: name.map(str::to_string),
        artist_name: None,
        known_external_ids: Vec::new(),
        library_types: Vec::new(),
    }
}

pub fn artist_query_with_known(
    mbid: Option<&str>,
    name: Option<&str>,
    known: &[(&str, &str)],
) -> EntityQuery {
    EntityQuery {
        known_external_ids: known
            .iter()
            .map(|(p, id)| FetchedId {
                provider: p.to_string(),
                external_id: id.to_string(),
                url: None,
            })
            .collect(),
        ..artist_query(mbid, name)
    }
}

pub const BEATLES_MBID: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d260c2";
pub const ABBEY_RG_MBID: &str = "b588555c-e1a7-45b1-8f91-2048a8393b58";
pub const ABBEY_RELEASE_MBID: &str = "22222222-2222-2222-2222-222222222222";

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试密钥对：私钥解码与公钥派生往返一致。
    #[test]
    fn test_keypair_decodes() {
        assert_eq!(test_signing_key().verifying_key(), test_verifying_key());
    }

    /// pack → verify 全链路自洽：最小合法清单 + 最小 wasm 模块（`\0asm` 头）。
    #[test]
    fn pack_verify_roundtrip() {
        let manifest = br#"{
            "id": "tma.test.echo",
            "name": "Echo",
            "version": "0.1.0",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 6 } },
            "extension_points": ["scrape_provider"],
            "scrape": {
                "provider": "echo_images",
                "capabilities": ["image"],
                "requires_credentials": false
            }
        }"#;
        let wasm = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let packed = tma_plugin_sdk::pack(manifest, &wasm, &test_signing_key()).unwrap();
        let verified = verify(&packed).unwrap();
        assert_eq!(verified.manifest.id, "tma.test.echo");
        assert_eq!(verified.wasm, wasm);
    }

    /// 未命中路由：`code:"network"` + 错误文案与宿主 `ProxyHttpError::Network`
    /// Display 同文，且请求仍被记录（断言行可见未命中 URL）。
    #[test]
    fn stub_proxy_miss_records_and_reports_network() {
        let proxy = StubProxy::new(vec![]);
        let resp = proxy.dispatch(
            "GET".into(),
            "https://example.com/x".into(),
            BTreeMap::new(),
            None,
        );
        assert!(!resp.ok);
        assert_eq!(resp.code.as_deref(), Some("network"));
        assert_eq!(
            resp.error.as_deref(),
            Some("network: stub 代理无预设响应: GET https://example.com/x")
        );
        assert_eq!(proxy.urls(), ["https://example.com/x"]);
    }

    /// capability 桩命中：返回注入响应并把完整请求 DTO 留痕。
    #[test]
    fn capability_stub_hit_responds_and_records() {
        let stub = CapabilityStub::authenticated("u-1", "alice", true).with_catalog(
            CatalogMatch::kind("artist"),
            catalog_ok(vec![catalog_item("a-1", "artist", "Artist A")]),
        );

        let resp = stub
            .dispatch_catalog(CatalogReadRequest {
                version: CAPABILITY_DTO_VERSION,
                kind: Some("artist".into()),
                limit: 20,
                ..Default::default()
            })
            .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.items[0].id, "a-1");

        let recorded = stub.catalog_requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].kind.as_deref(), Some("artist"));
        assert_eq!(recorded[0].limit, 20);

        let identity = stub
            .dispatch_identity(IdentityReadRequest {
                version: CAPABILITY_DTO_VERSION,
            })
            .unwrap();
        assert!(identity.ok && identity.is_admin);
        assert_eq!(identity.username.as_deref(), Some("alice"));
        assert_eq!(stub.identity_requests().len(), 1);
    }

    /// capability 桩未配置/未命中：宿主函数级 `Err`（消息带 capability 名与
    /// 请求摘要），但请求仍先留痕——与 StubProxy 未命中同精神。
    #[test]
    fn capability_stub_miss_errors_and_still_records() {
        let stub = CapabilityStub::new();

        let err = stub
            .dispatch_catalog(CatalogReadRequest {
                kind: Some("song".into()),
                id: Some("s-1".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("tma_catalog_read"), "{err}");
        assert!(err.to_string().contains("s-1"), "{err}");
        assert_eq!(stub.catalog_requests().len(), 1);

        let err = stub
            .dispatch_identity(IdentityReadRequest { version: 1 })
            .unwrap_err();
        assert!(err.to_string().contains("tma_identity_read"), "{err}");
        assert_eq!(stub.identity_requests().len(), 1);

        let err = stub
            .dispatch_media(MediaStreamRequest {
                version: 1,
                media_id: "m-1".into(),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("tma_media_stream"), "{err}");
        assert_eq!(stub.media_requests().len(), 1);

        // 路由表非空但字段不匹配同样走宿主级 Err。
        let stub =
            CapabilityStub::new().with_catalog(CatalogMatch::kind("artist"), catalog_ok(vec![]));
        assert!(
            stub.dispatch_catalog(CatalogReadRequest {
                kind: Some("album".into()),
                ..Default::default()
            })
            .is_err()
        );
    }

    /// 字段匹配是「全部约束同时满足」：多约束路由不被部分命中。
    #[test]
    fn capability_stub_match_requires_all_set_fields() {
        let stub = CapabilityStub::new().with_media(
            MediaMatch::media_id("m-1").and_codec("flac"),
            media_ok(200, "audio/flac", None),
        );
        // codec 不命中 → Err。
        assert!(
            stub.dispatch_media(MediaStreamRequest {
                media_id: "m-1".into(),
                codec: Some("mp3".into()),
                ..Default::default()
            })
            .is_err()
        );
        // 全约束命中 → ok。
        let resp = stub
            .dispatch_media(MediaStreamRequest {
                media_id: "m-1".into(),
                codec: Some("flac".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(resp.ok);
        assert_eq!(stub.media_requests().len(), 2, "未命中也留痕");
    }

    /// 权限门放行集只收 `capability` 类权限，`http`/`ai` 不影响。
    #[test]
    fn granted_capabilities_extracts_capability_permissions_only() {
        let manifest: PluginManifest = serde_json::from_str(
            r#"{
                "id": "tma.test.echo",
                "name": "Echo",
                "version": "0.1.0",
                "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 6 } },
                "extension_points": ["scrape_provider"],
                "scrape": {
                    "provider": "echo_images",
                    "capabilities": ["image"],
                    "requires_credentials": false
                },
                "permissions": [
                    { "capability": { "name": "catalog.read", "reason": "r" } },
                    { "capability": { "name": "media.stream", "reason": "r" } },
                    { "http": { "host": "example.com", "reason": "r" } }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            granted_capabilities(&manifest),
            ["catalog.read", "media.stream"]
                .into_iter()
                .map(String::from)
                .collect()
        );
    }

    /// 逐次权限门：验签包路径（granted=Some）对清单未声明的 capability 判
    /// denied，宿主函数写回 `ok:false` + `forbidden` 应答（media 为 403）；
    /// 裸 wasm 路径（granted=None）保持放行。
    #[test]
    fn capability_grant_gate_denies_undeclared() {
        let state = |granted: Option<BTreeSet<String>>| HostFnState {
            proxy: StubProxy::new(vec![]),
            capabilities: CapabilityStub::new(),
            runtime_config: Arc::from("{}"),
            granted: granted.map(Arc::new),
        };
        let declared: BTreeSet<String> = ["catalog.read".to_string()].into_iter().collect();

        assert!(!capability_denied(
            &state(Some(declared.clone())),
            "catalog.read"
        ));
        assert!(capability_denied(&state(Some(declared)), "media.stream"));
        // 无清单路径（from_wasm*）不做权限门。
        assert!(!capability_denied(&state(None), "media.stream"));
    }

    /// respond 段的校验顺序与宿主逐分支一致，直接打三条 respond 函数：
    /// 入参损坏 → `bad_request`；DTO 版本不符 → `unsupported_version`；
    /// 权限门在字段校验之后（media 非法 range 416 先于未声明 403；
    /// catalog 未声明 `forbidden` 先于 `limit` 越界 bad_request）；
    /// 被前置校验/权限门拒绝的请求不进入桩派发、不留痕。
    #[test]
    fn capability_responders_validate_in_host_order() {
        let stub = CapabilityStub::authenticated("u-1", "alice", false);
        let state = |granted: BTreeSet<String>| HostFnState {
            proxy: StubProxy::new(vec![]),
            capabilities: stub.clone(),
            runtime_config: Arc::from("{}"),
            granted: Some(Arc::new(granted)),
        };
        let all =
            |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| n.to_string()).collect() };
        let media_req = |range: Option<MediaByteRange>| {
            Ok(MediaStreamRequest {
                version: CAPABILITY_DTO_VERSION,
                media_id: "m-1".into(),
                range,
                ..Default::default()
            })
        };

        // 入参损坏：三条 respond 都是 bad_request（media 状态 400）。
        let resp = media_respond(&state(all(&[])), Err("坏 JSON".into())).unwrap();
        assert!(!resp.ok && resp.status == 400);
        assert_eq!(resp.error.as_ref().unwrap().code, "bad_request");
        let resp = catalog_respond(&state(all(&[])), Err("坏 JSON".into())).unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "bad_request");
        let resp = identity_respond(&state(all(&[])), Err("坏 JSON".into())).unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "bad_request");

        // DTO 版本不符 → unsupported_version，先于权限门。
        let mut bad_version = media_req(None).unwrap();
        bad_version.version = CAPABILITY_DTO_VERSION + 1;
        let resp = media_respond(&state(all(&[])), Ok(bad_version)).unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "unsupported_version");
        assert_eq!(resp.status, 400);
        let resp = catalog_respond(
            &state(all(&[])),
            Ok(CatalogReadRequest {
                version: CAPABILITY_DTO_VERSION + 1,
                limit: 20,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "unsupported_version");

        // media：字段校验（media_id 空 → 400；非法 range → 416）先于权限门。
        let mut empty_id = media_req(None).unwrap();
        empty_id.media_id = "  ".into();
        let resp = media_respond(&state(all(&[])), Ok(empty_id)).unwrap();
        assert_eq!(resp.status, 400);
        assert_eq!(resp.error.as_ref().unwrap().code, "bad_request");
        let bad_range = media_req(Some(MediaByteRange {
            start: 10,
            end: Some(5),
        }));
        let resp = media_respond(&state(all(&[])), bad_range).unwrap();
        assert_eq!(resp.status, 416, "非法 range 先于权限门拒绝");
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_range");

        // media：通过字段校验但未声明 media.stream → 403 forbidden。
        let resp = media_respond(&state(all(&["catalog.read"])), media_req(None)).unwrap();
        assert_eq!(resp.status, 403);
        assert_eq!(resp.error.as_ref().unwrap().code, "forbidden");

        // catalog：权限门先于 limit 校验——未声明时即使 limit 越界也先 403。
        let resp = catalog_respond(
            &state(all(&["media.stream"])),
            Ok(CatalogReadRequest {
                version: CAPABILITY_DTO_VERSION,
                limit: 0,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "forbidden");

        // catalog：声明后 limit 越界 → bad_request。
        let resp = catalog_respond(
            &state(all(&["catalog.read"])),
            Ok(CatalogReadRequest {
                version: CAPABILITY_DTO_VERSION,
                limit: 0,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "bad_request");

        // identity：未声明 → forbidden。
        let resp = identity_respond(
            &state(all(&["catalog.read"])),
            Ok(IdentityReadRequest {
                version: CAPABILITY_DTO_VERSION,
            }),
        )
        .unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, "forbidden");

        // 以上全部在派发前被拒，桩的留痕为空；放行后正常进桩（未配置 → Err 且留痕）。
        assert!(stub.media_requests().is_empty());
        assert!(stub.catalog_requests().is_empty());
        assert!(stub.identity_requests().is_empty());
        assert!(
            media_respond(&state(all(&["media.stream"])), media_req(None),).is_err(),
            "放行但未配置桩 → 宿主级 Err"
        );
        assert_eq!(stub.media_requests().len(), 1, "进派发的调用才留痕");
    }
}
