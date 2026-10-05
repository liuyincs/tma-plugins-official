//! 插件 ABI 契约：扩展点输入输出纯数据 DTO 的单一之家。
//!
//! 插件系统目标形态是 WASM 沙箱（宿主 Rust，wasmtime/Extism）。本 crate 不引入
//! WASM 运行时，只把"跨插件边界能传什么"先收敛成版本化的纯数据契约：
//!
//! - [`abi`]：ABI 版本区间与兼容判定（min/max 双端，规避只有下界的版本地牢）；
//! - [`manifest`]：插件清单 schema（golden 夹具在 `fixtures/`）；
//! - [`message`]：宿主↔插件的信封式请求/响应与纯数据错误；
//! - [`scrape`]：刮削扩展点的 wire DTO（不含宿主 pipeline 专用字段）；
//! - [`events`]：事件订阅扩展点的 wire DTO（ABI 1.3 起，`tma_event` 导出）；
//! - [`library_type`]：资料库类型 wire 闭集（ABI 1.5 起，清单与载荷共用）；
//! - [`playlist_import`]：歌单导入扩展点的 wire DTO（ABI 1.4 起，
//!   `tma_playlist_import` 导出）；
//! - [`http_host`] / [`ai_host`]：宿主函数 `http_request` / `ai_chat` 的跨边界
//!   JSON 协议（出站 HTTP 与单轮 AI 的默认拒绝边界见 [`permission`]）；
//! - [`http_route`]：通用入站 HTTP 扩展点 `tma_http` 的路由声明与请求/响应 DTO
//!   （ABI 1.6 起）；
//! - [`permission`]：权限声明（ABI 1.1 起的默认拒绝边界）；
//! - [`guest`]：guest 侧共享工具（出站 HTTP 封装/配置解析/URL 编码，宿主可单测）。
//!
//! 宿主运行时 DTO 不在本 crate（属宿主侧 provider/storage 层）。

pub mod abi;
pub mod ai_host;
pub mod capability;
pub mod events;
pub mod guest;
pub mod http_host;
pub mod http_route;
pub mod library_type;
#[doc(hidden)]
mod macros;
pub mod manifest;
pub mod matching;
pub mod message;
pub mod normalize;
#[cfg(feature = "package")]
pub mod package;
pub mod permission;
pub mod playlist_import;
pub mod scrape;
pub mod version;

/// wasm 插件宏使用的 Extism PDK 路径。宿主 target 不会解析该模块或依赖。
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod __rt {
    pub use extism_pdk;
}

pub use abi::{
    ACTIONS_MIN_ABI, AbiRange, AbiVersion, EVENTS_MIN_ABI, HOST_ABI, HTTP_MIN_ABI,
    LIBRARY_TYPES_MIN_ABI, PLAYLIST_IMPORT_MIN_ABI,
};
pub use ai_host::{AiChatHostRequest, AiChatHostResponse};
pub use capability::{
    CAPABILITY_DTO_VERSION, CapabilityError, CatalogItem, CatalogReadRequest, CatalogReadResponse,
    IdentityReadRequest, IdentityReadResponse, MediaByteRange, MediaClientCapabilities,
    MediaStreamRequest, MediaStreamResponse,
};
pub use events::{
    NowPlayingEventPayload, PluginEventKind, PluginEventRequest, PluginEventResponse,
    ScrobbleEventPayload, TrackEventFields,
};
pub use guest::{
    DEFAULT_USER_AGENT, GuestHttp, HttpTransport, MIN_RETRY_AFTER_SECS, MIN_SCROBBLE_SECONDS,
    RawResponse, base64_decode, base64_encode, host_failure_to_error, join_artists, load_config,
    parse_retry_after, parse_retry_after_seconds, percent_encode_query, should_skip_scrobble,
    status_to_error, truncate_hint,
};
pub use http_host::{HttpHostRequest, HttpHostResponse};
pub use http_route::{
    HttpManifest, HttpRouteManifest, HttpRouteMethod, PluginHttpIdentity, PluginHttpRequest,
    PluginHttpResponse,
};
pub use library_type::{
    DEFAULT_LIBRARY_TYPES, LibraryType, covers, covers_one, resolve_library_types,
};
pub use manifest::{
    ActionManifest, BUILTIN_PLUGIN_ID_PREFIX, ConfigValuesError, ManifestError,
    PlaylistImportManifest, PlaylistImportMethod, PluginManifest, ScrapeManifest,
    ScrobbleReporterManifest, is_builtin_plugin_id, validate_config_values,
};
pub use matching::{fuzzy_confidence, levenshtein, name_similarity};
pub use message::{
    PluginActionRequest, PluginActionResponse, PluginError, PluginErrorCode, PluginOp,
    PluginOutcome, PluginRequest, PluginResponse, RateLimitRetry,
};
pub use normalize::{normalize_mbid, normalize_name};
#[cfg(feature = "package")]
pub use package::{PackageError, PluginIcon, VerifiedPlugin, pack, pack_with_icon, verify};
pub use permission::{
    FALLBACK_TRAFFIC_GROUP_PREFIX, HttpScheme, HttpTrafficPolicy, INBOUND_CAPABILITIES, Permission,
    PermissionError,
};
pub use playlist_import::{
    PlaylistImportRequest, PlaylistImportResponse, PlaylistImportTrack, join_artist_names,
};
pub use scrape::{
    Bio, EntityQuery, FetchedId, RemoteExtraImage, RemoteImage, ScrapeCapability, ScrapeEntityKind,
    ScrapeResult, ScrapedAlias, ScrapedOfficialAlbum, ScrapedRecording,
};
pub use version::{VersionError, compare_versions, parse_version};
