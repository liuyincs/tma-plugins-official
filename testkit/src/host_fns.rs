//! 宿主函数桩（注册到 extism 默认 `extism:host/user` 命名空间）。

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use extism::{CurrentPlugin, UserData, Val};

use crate::{
    CAPABILITY_DTO_VERSION, CapabilityError, CapabilityStub, CatalogReadRequest,
    CatalogReadResponse, HttpHostRequest, HttpHostResponse, IdentityReadRequest,
    IdentityReadResponse, MediaStreamRequest, MediaStreamResponse, PluginManifest, StubProxy,
};

// ---------------------------------------------------------------------------
// 宿主函数桩（注册到 extism 默认 `extism:host/user` 命名空间）
// ---------------------------------------------------------------------------

/// 宿主函数 user data：stub 代理 + capability 桩 + 注入的运行时配置 JSON +
/// 清单声明的 capability 授权集合。
#[derive(Clone)]
pub(crate) struct HostFnState {
    pub(crate) proxy: Arc<StubProxy>,
    pub(crate) capabilities: Arc<CapabilityStub>,
    pub(crate) runtime_config: Arc<str>,
    /// `Some` = 验签包路径，只放行 manifest `permissions` 声明过的
    /// capability；`None` = 无清单路径（`from_wasm*`），全部放行。
    pub(crate) granted: Option<Arc<BTreeSet<String>>>,
}

/// capability 权限门前的判定（对应宿主 `gate::capability_allowed`）：
/// `granted` 为 `Some` 时放行清单声明过的 capability 名；`None`（裸 wasm
/// 路径）全放行。
pub(crate) fn capability_denied(state: &HostFnState, capability: &str) -> bool {
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
pub(crate) fn granted_capabilities(manifest: &PluginManifest) -> BTreeSet<String> {
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
pub(crate) fn http_request_host_fn(
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
pub(crate) fn tma_config_host_fn(
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

/// capability 入参 JSON 解析（宿主 `read_capability_input` 的收尾一段）：
/// 生产路径（[`capability_json_call`]）与契约回放（`contract::parse_input`）
/// 共用同一入口——回放走与跨内存边界完全相同的 `from_slice` 语义，失败
/// 文案同式，解析行为漂移时夹具必红。
pub(crate) fn parse_capability_input<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("入参 JSON 非法: {e}"))
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
        parse_capability_input(&bytes)
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
pub(crate) fn tma_catalog_read_host_fn(
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
pub(crate) fn tma_identity_read_host_fn(
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
pub(crate) fn tma_media_stream_host_fn(
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
pub(crate) fn catalog_respond(
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
pub(crate) fn identity_respond(
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
pub(crate) fn media_respond(
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

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
}
