//! 内置刮削插件（`plugins/builtin/*`）的 wasm FFI 样板宏。
//!
//! 8 个内置插件曾各自复制同一份约 60-70 行样板：`MANIFEST` 常量与
//! `tma_manifest()` 导出、`http_request`/`tma_config` 宿主函数声明、`scrape`
//! 入口的 op 分发与信封包装、`transport()` 闭包。本 crate 用
//! [`scrape_plugin!`] / [`plugin!`] 声明宏把它们收敛为一处，插件只保留业务函数：
//!
//! ```ignore
//! tma_plugin_sdk::plugin! {
//!     manifest = "../manifest.json",   // include_str! 相对调用文件（src/lib.rs）
//!     slug = "apple_music",            // 仅用于 browse_discography 的 Unsupported 文案
//!     scrape = run_scrape,             // 可选：fn(EntityQuery) -> Result<ScrapeResult, PluginError>
//!     browse = browse_discography,     // 可选：fn(&str) -> Result<Vec<ScrapedOfficialAlbum>, PluginError>
//!     config = read_config,            // 可选：声明 tma_config 宿主函数并生成指定名的 read 助手
//!     actions = run_action,            // 可选：生成 tma_action 导出（ABI 1.2 起；ident 或 path）
//!     event = handle_event,            // 可选：生成 tma_event 导出（ABI 1.3 起）
//!     import = run_import,             // 可选：生成 tma_playlist_import 导出（ABI 1.4 起）
//!     http = run_http,                 // 可选：生成 tma_http 导出（ABI 1.6 起）
//!     ai_chat,                         // 可选：声明 ai_chat 宿主函数（与 import 独立）
//!     catalog_read = catalog,          // 可选：声明 tma_catalog_read 宿主函数并生成助手（ABI 1.6 起）
//!     identity_read = identity,        // 可选：声明 tma_identity_read 宿主函数并生成助手（ABI 1.6 起）
//!     media_stream = media_stream,     // 可选：声明 tma_media_stream 宿主函数并生成助手（ABI 1.6 起）
//! }
//! ```
//!
//! `import = f`（ABI 1.4 歌单导入扩展点，见 `tma_plugin_sdk::playlist_import`）
//! 只生成 `tma_playlist_import` 导出：把 `Json<PlaylistImportRequest>` 拆包喂给
//! `f(PlaylistImportRequest) -> Result<PlaylistImportResponse, PluginError>`，
//! `Ok` 序列化回宿主，`Err` 折叠为**非零退出码**（跨边界只传退出码，
//! `PluginError.message` 只能进插件侧日志）。`ai_chat` 片段单独声明宿主函数
//! （`extism:host/user`，供插件自行包装成调用助手）；两者可只写其一，也可同时
//! 声明（内置 `tma.builtin.playlist_import` 即 `import` + `ai_chat`）。`ai_chat`
//! 须写在 capability 槽（`catalog_read`/`identity_read`/`media_stream`）之前。
//!
//! `http = f`（ABI 1.6 入站 HTTP 扩展点，见 `tma_plugin_sdk::http_route`）
//! 只生成 `tma_http` 导出：`f(PluginHttpRequest) -> Result<PluginHttpResponse,
//! PluginError>`，`Err` 折叠口径与 `import` 一致（非零退出码）。
//! `catalog_read`/`identity_read`/`media_stream`（ABI 1.6 capability 宿主函数，
//! 见 `tma_plugin_sdk::capability`）各自声明 `tma_*` 宿主函数并生成指定名的
//! 调用助手：请求 DTO → unsafe 调宿主函数 → 响应 DTO；宿主调用失败折叠为
//! `PluginError`（与 `config` 助手同口径），业务错误仍走响应 DTO 的
//! `ok`/`error` 字段。capability 槽保持最后。
//!
//! 未声明 `scrape` 时仍导出 `scrape`（宿主 ABI 探测），但业务恒为
//! `Unsupported`——事件/动作专用插件不必手写 dummy `run_scrape`。
//!
//! 宏展开先注入 `use $crate::__rt::extism_pdk;`（SDK 仅在 wasm32 target 下
//! 经 `__rt` 再导出 extism-pdk），宏体内 `extism_pdk::` 无前缀路径与
//! extism-pdk proc macro（`plugin_fn`/`host_fn`）吐出的无前缀路径都由该
//! `use` 解析；`$crate` 对插件侧 dep 重命名免疫。**插件 crate 只需依赖
//! `tma-plugin-sdk`**；手写 host 函数的插件经
//! `tma_plugin_sdk::__rt::extism_pdk` 拿同一入口。插件 crate 根仍需自带
//! `#![cfg(target_arch = "wasm32")]`（crate 级属性无法由宏吐出）。

/// 与 [`scrape_plugin!`] 同义（更贴非刮削插件的语义）。
#[macro_export]
macro_rules! plugin {
    ($($tt:tt)*) => {
        $crate::scrape_plugin! { $($tt)* }
    };
}

/// 吐出内置插件的全部 FFI 样板。各片段语义与手写样板逐字一致：
///
/// - `MANIFEST` 常量 + `tma_manifest()` 导出（安装期探测，包内清单同源）；
/// - `http_request` 宿主函数声明与 `transport()` 包装（[`GuestHttp`] 闭包形状）；
/// - `scrape` 导出（可选业务）：声明 `scrape = f` 时按 op 分发；未声明时
///   仍导出但恒 `Unsupported`（与无 `browse` 分支文案风格一致）；
/// - `config` / `actions` / `event` / `import` / `http` / `ai_chat` /
///   `catalog_read` / `identity_read` / `media_stream` 等同 [`plugin!`] 模块文档。
///
/// [`GuestHttp`]: https://docs.rs/tma-plugin-sdk
#[macro_export]
macro_rules! scrape_plugin {
    // `ai_chat` 须写在 capability 槽之前（见模块文档示例）；capability 槽保持最后。
    (
        manifest = $manifest:literal,
        slug = $slug:literal
        $(, scrape = $scrape:ident)?
        $(, browse = $browse:ident)?
        $(, config = $config:ident)?
        $(, actions = $actions:path)?
        $(, event = $event:ident)?
        $(, import = $import:ident)?
        $(, http = $http:ident)?
        , ai_chat
        $(, catalog_read = $catalog_read:ident)?
        $(, identity_read = $identity_read:ident)?
        $(, media_stream = $media_stream:ident)?
        $(,)?
    ) => {
        $crate::scrape_plugin! {
            @emit
            manifest = $manifest,
            slug = $slug
            $(, scrape = $scrape)?
            $(, browse = $browse)?
            $(, config = $config)?
            $(, actions = $actions)?
            $(, event = $event)?
            $(, import = $import)?
            $(, http = $http)?
            , @ai_chat = yes
            $(, catalog_read = $catalog_read)?
            $(, identity_read = $identity_read)?
            $(, media_stream = $media_stream)?
        }
    };
    (
        manifest = $manifest:literal,
        slug = $slug:literal
        $(, scrape = $scrape:ident)?
        $(, browse = $browse:ident)?
        $(, config = $config:ident)?
        $(, actions = $actions:path)?
        $(, event = $event:ident)?
        $(, import = $import:ident)?
        $(, http = $http:ident)?
        $(, catalog_read = $catalog_read:ident)?
        $(, identity_read = $identity_read:ident)?
        $(, media_stream = $media_stream:ident)?
        $(,)?
    ) => {
        $crate::scrape_plugin! {
            @emit
            manifest = $manifest,
            slug = $slug
            $(, scrape = $scrape)?
            $(, browse = $browse)?
            $(, config = $config)?
            $(, actions = $actions)?
            $(, event = $event)?
            $(, import = $import)?
            $(, http = $http)?
            , @ai_chat = no
            $(, catalog_read = $catalog_read)?
            $(, identity_read = $identity_read)?
            $(, media_stream = $media_stream)?
        }
    };

    (
        @emit
        manifest = $manifest:literal,
        slug = $slug:literal
        $(, scrape = $scrape:ident)?
        $(, browse = $browse:ident)?
        $(, config = $config:ident)?
        $(, actions = $actions:path)?
        $(, event = $event:ident)?
        $(, import = $import:ident)?
        $(, http = $http:ident)?
        , @ai_chat = $ai_chat:ident
        $(, catalog_read = $catalog_read:ident)?
        $(, identity_read = $identity_read:ident)?
        $(, media_stream = $media_stream:ident)?
    ) => {
        /// extism-pdk 入口（本 crate 不直依，经 `__rt` 再导出）；proc macro
        /// 吐出的无前缀 `extism_pdk::` 路径由本 `use` 解析。
        use $crate::__rt::extism_pdk;

        /// 与包内 `manifest.json` 同源（安装期探测要求语义一致）。
        const MANIFEST: &str = include_str!($manifest);

        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            fn http_request(
                input: extism_pdk::Json<$crate::HttpHostRequest>,
            ) -> extism_pdk::Json<$crate::HttpHostResponse>;
        }

        $crate::scrape_plugin!(@config $($config)?);

        $crate::scrape_plugin!(@actions $($actions)?);

        $crate::scrape_plugin!(@event $($event)?);

        $crate::scrape_plugin!(@import $($import)?);

        $crate::scrape_plugin!(@http $($http)?);

        $crate::scrape_plugin!(@ai_chat_flag $ai_chat);

        $crate::scrape_plugin!(@catalog_read $($catalog_read)?);

        $crate::scrape_plugin!(@identity_read $($identity_read)?);

        $crate::scrape_plugin!(@media_stream $($media_stream)?);

        /// 安装期探测：返回包内同款清单。
        #[extism_pdk::plugin_fn]
        pub fn tma_manifest() -> extism_pdk::FnResult<String> {
            Ok(MANIFEST.to_string())
        }

        /// 单实体刮削 / 碟谱 browse（同一导出按 op 分发；未声明 scrape 时恒 Unsupported）。
        #[extism_pdk::plugin_fn]
        pub fn scrape(
            input: extism_pdk::Json<$crate::PluginRequest>,
        ) -> extism_pdk::FnResult<extism_pdk::Json<$crate::PluginResponse>> {
            let req = input.0;
            let resp = match run(req.op) {
                Ok(outcome) => $crate::PluginResponse {
                    id: req.id,
                    outcome,
                },
                Err(e) => $crate::PluginResponse {
                    id: req.id,
                    outcome: $crate::PluginOutcome::Error(e),
                },
            };
            Ok(extism_pdk::Json(resp))
        }

        $crate::scrape_plugin!(@run $slug $(, $scrape)? $(, $browse)?);

        /// 出站传输：把 extism `http_request` 宿主函数包成 `GuestHttp` 闭包形状。
        fn transport(
            req: $crate::HttpHostRequest,
        ) -> Result<$crate::HttpHostResponse, $crate::PluginError> {
            unsafe { http_request(extism_pdk::Json(req)) }
                .map(|extism_pdk::Json(resp)| resp)
                .map_err(|e| {
                    $crate::PluginError::new(
                        $crate::PluginErrorCode::Internal,
                        format!("{e:?}"),
                    )
                })
        }
    };

    // 声明 scrape：按 op 分发到业务函数。
    (@run $slug:literal, $scrape:ident $(, $browse:ident)?) => {
        fn run(
            op: $crate::PluginOp,
        ) -> Result<$crate::PluginOutcome, $crate::PluginError> {
            match op {
                $crate::PluginOp::Scrape { query } => Ok(
                    $crate::PluginOutcome::Scrape(Box::new($scrape(query)?)),
                ),
                $crate::PluginOp::BrowseDiscography { artist_mbid } => {
                    $crate::scrape_plugin!(@browse $slug, artist_mbid $(, $browse)?)
                }
            }
        }
    };
    // 未声明 scrape：scrape 导出仍存在，业务恒 Unsupported。
    (@run $slug:literal) => {
        fn run(
            op: $crate::PluginOp,
        ) -> Result<$crate::PluginOutcome, $crate::PluginError> {
            match op {
                $crate::PluginOp::Scrape { .. } => Err(
                    $crate::PluginError::new(
                        $crate::PluginErrorCode::Unsupported,
                        concat!($slug, " 不支持 scrape 操作").into(),
                    ),
                ),
                $crate::PluginOp::BrowseDiscography { artist_mbid } => {
                    $crate::scrape_plugin!(@browse $slug, artist_mbid)
                }
            }
        }
    };

    // 支持碟谱 browse：分发到插件业务函数。
    (@browse $slug:literal, $artist_mbid:ident, $browse:ident) => {
        Ok($crate::PluginOutcome::Discography($browse(&$artist_mbid)?))
    };
    // 不支持 browse：Unsupported（文案带 slug，与原各插件手写串逐字一致）。
    (@browse $slug:literal, $artist_mbid:ident) => {{
        let _ = $artist_mbid;
        Err($crate::PluginError::new(
            $crate::PluginErrorCode::Unsupported,
            concat!($slug, " 不支持 browse_discography 操作").into(),
        ))
    }};

    // 需要运行时配置的插件：追加 tma_config 宿主函数声明与指定名的配置读取助手。
    (@config $config:ident) => {
        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            /// 无参：返回本插件运行时配置的 JSON 字符串（values + 解密后 secrets 合并；
            /// 未配置为 `{}`）。契约详见宿主侧 engine 模块注释。
            fn tma_config() -> String;
        }

        fn $config() -> Result<String, $crate::PluginError> {
            unsafe { tma_config() }.map_err(|e| {
                $crate::PluginError::new(
                    $crate::PluginErrorCode::Internal,
                    format!("tma_config 宿主函数调用失败: {e:?}"),
                )
            })
        }
    };
    // 无配置插件：不声明 tma_config，wasm 导入表不含该项。
    (@config) => {};

    // 声明动作：生成 tma_action 导出。
    (@actions $actions:path) => {
        /// 动作入口（可选导出，manifest `actions` 配套）：请求/响应信封拆装。
        #[extism_pdk::plugin_fn]
        pub fn tma_action(
            input: extism_pdk::Json<$crate::PluginActionRequest>,
        ) -> extism_pdk::FnResult<extism_pdk::Json<$crate::PluginActionResponse>> {
            let resp = match $actions(input.0) {
                Ok(resp) => resp,
                Err(e) => $crate::PluginActionResponse::failure(e.message),
            };
            Ok(extism_pdk::Json(resp))
        }
    };
    (@actions) => {};

    // 声明事件订阅：生成 tma_event 导出。
    (@event $event:ident) => {
        /// 事件入口（可选导出，manifest `scrobble_reporter` 配套）：请求/响应信封拆装。
        #[extism_pdk::plugin_fn]
        pub fn tma_event(
            input: extism_pdk::Json<$crate::PluginEventRequest>,
        ) -> extism_pdk::FnResult<extism_pdk::Json<$crate::PluginEventResponse>> {
            let resp = match $event(input.0) {
                Ok(resp) => resp,
                Err(e) => $crate::PluginEventResponse::rejected(e),
            };
            Ok(extism_pdk::Json(resp))
        }
    };
    (@event) => {};

    // 声明 ai_chat 宿主函数（与 import 独立；见模块文档 `ai_chat` 片段）。
    (@ai_chat_flag yes) => {
        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            /// 单轮 AI 对话（ABI 1.4）：契约见 `tma-plugin-sdk::ai_host`（无会话
            /// 状态、无 max_tokens 字段；清单须声明 `Permission::Ai`，宿主逐次
            /// 校验）。插件自行把它包装成调用助手。
            fn ai_chat(
                input: extism_pdk::Json<$crate::AiChatHostRequest>,
            ) -> extism_pdk::Json<$crate::AiChatHostResponse>;
        }
    };
    (@ai_chat_flag no) => {};

    // 声明歌单导入：生成 tma_playlist_import 导出（不连带 ai_chat）。
    (@import $import:ident) => {
        /// 歌单导入入口（可选导出，manifest `playlist_import` 配套）：请求 DTO
        /// 拆包 → 业务函数 → 响应序列化；业务失败折叠为非零退出码（跨边界只传
        /// 退出码，`PluginError.message` 进不了宿主，宿主统一按「插件调用失败」
        /// 分类）。
        #[extism_pdk::plugin_fn]
        pub fn tma_playlist_import(
            input: extism_pdk::Json<$crate::PlaylistImportRequest>,
        ) -> extism_pdk::FnResult<extism_pdk::Json<$crate::PlaylistImportResponse>>
        {
            match $import(input.0) {
                Ok(resp) => Ok(extism_pdk::Json(resp)),
                Err(e) => Err(extism_pdk::WithReturnCode::new(
                    extism_pdk::Error::msg(e.message),
                    1,
                )),
            }
        }
    };
    // 无歌单导入：不导出 tma_playlist_import（宿主探测得到 Unsupported）。
    (@import) => {};

    // 声明入站 HTTP：生成 tma_http 导出（ABI 1.6 起）。
    (@http $http:ident) => {
        /// 入站 HTTP 入口（可选导出，manifest `http.routes` 配套）：请求 DTO
        /// 拆包 → 业务函数 → 响应序列化；业务失败折叠为非零退出码（与
        /// `tma_playlist_import` 同口径：跨边界只传退出码，`PluginError.message`
        /// 进不了宿主）。
        #[extism_pdk::plugin_fn]
        pub fn tma_http(
            input: extism_pdk::Json<$crate::PluginHttpRequest>,
        ) -> extism_pdk::FnResult<extism_pdk::Json<$crate::PluginHttpResponse>>
        {
            match $http(input.0) {
                Ok(resp) => Ok(extism_pdk::Json(resp)),
                Err(e) => Err(extism_pdk::WithReturnCode::new(
                    extism_pdk::Error::msg(e.message),
                    1,
                )),
            }
        }
    };
    // 无入站 HTTP：不导出 tma_http（宿主探测得到 Unsupported）。
    (@http) => {};

    // ABI 1.6 capability：声明 tma_catalog_read 宿主函数并生成指定名读取助手
    // （清单须声明 `catalog.read` 权限，宿主逐次校验）。
    (@catalog_read $catalog_read:ident) => {
        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            /// 曲库只读查询（ABI 1.6）：契约见 `tma-plugin-sdk::capability`。
            fn tma_catalog_read(
                input: extism_pdk::Json<$crate::CatalogReadRequest>,
            ) -> extism_pdk::Json<$crate::CatalogReadResponse>;
        }

        /// 调宿主 `tma_catalog_read`：宿主调用失败折叠为 `PluginError`；业务
        /// 错误仍在响应 DTO 的 `ok`/`error` 字段（此处不折叠）。
        fn $catalog_read(
            req: $crate::CatalogReadRequest,
        ) -> Result<$crate::CatalogReadResponse, $crate::PluginError> {
            unsafe { tma_catalog_read(extism_pdk::Json(req)) }
                .map(|extism_pdk::Json(resp)| resp)
                .map_err(|e| {
                    $crate::PluginError::new(
                        $crate::PluginErrorCode::Internal,
                        format!("tma_catalog_read 宿主函数调用失败: {e:?}"),
                    )
                })
        }
    };
    // 未声明 catalog.read：不声明 tma_catalog_read，wasm 导入表不含该项。
    (@catalog_read) => {};

    // ABI 1.6 capability：声明 tma_identity_read 宿主函数并生成指定名读取助手
    // （清单须声明 `identity.read` 权限，宿主逐次校验）。
    (@identity_read $identity_read:ident) => {
        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            /// 当前调用身份快照（ABI 1.6）：契约见 `tma-plugin-sdk::capability`。
            fn tma_identity_read(
                input: extism_pdk::Json<$crate::IdentityReadRequest>,
            ) -> extism_pdk::Json<$crate::IdentityReadResponse>;
        }

        /// 调宿主 `tma_identity_read`：宿主调用失败折叠为 `PluginError`；业务
        /// 错误仍在响应 DTO 的 `ok`/`error` 字段（此处不折叠）。
        fn $identity_read(
            req: $crate::IdentityReadRequest,
        ) -> Result<$crate::IdentityReadResponse, $crate::PluginError> {
            unsafe { tma_identity_read(extism_pdk::Json(req)) }
                .map(|extism_pdk::Json(resp)| resp)
                .map_err(|e| {
                    $crate::PluginError::new(
                        $crate::PluginErrorCode::Internal,
                        format!("tma_identity_read 宿主函数调用失败: {e:?}"),
                    )
                })
        }
    };
    // 未声明 identity.read：不声明 tma_identity_read，wasm 导入表不含该项。
    (@identity_read) => {};

    // ABI 1.6 capability：声明 tma_media_stream 宿主函数并生成指定名读取助手
    // （清单须声明 `media.stream` 权限，宿主逐次校验）。
    (@media_stream $media_stream:ident) => {
        #[extism_pdk::host_fn("extism:host/user")]
        extern "ExtismHost" {
            /// 按稳定 media id 读媒体流（ABI 1.6）：契约见
            /// `tma-plugin-sdk::capability`。
            fn tma_media_stream(
                input: extism_pdk::Json<$crate::MediaStreamRequest>,
            ) -> extism_pdk::Json<$crate::MediaStreamResponse>;
        }

        /// 调宿主 `tma_media_stream`：宿主调用失败折叠为 `PluginError`；业务
        /// 错误仍在响应 DTO 的 `ok`/`error`/`status` 字段（此处不折叠，
        /// 404/416 等状态需插件自行翻译成协议响应）。
        fn $media_stream(
            req: $crate::MediaStreamRequest,
        ) -> Result<$crate::MediaStreamResponse, $crate::PluginError> {
            unsafe { tma_media_stream(extism_pdk::Json(req)) }
                .map(|extism_pdk::Json(resp)| resp)
                .map_err(|e| {
                    $crate::PluginError::new(
                        $crate::PluginErrorCode::Internal,
                        format!("tma_media_stream 宿主函数调用失败: {e:?}"),
                    )
                })
        }
    };
    // 未声明 media.stream：不声明 tma_media_stream，wasm 导入表不含该项。
    (@media_stream) => {};
}
