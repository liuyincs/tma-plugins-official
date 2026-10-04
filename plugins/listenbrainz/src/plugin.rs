//! listenbrainz 插件的 wasm 本体（仅 wasm target 编译，见 `lib.rs` 模块布局注释）。
//!
//! 两块业务（纯逻辑在 `crate::scrobble`，双 target 编译可单测）：
//!
//! - **`tma_event` 处理器**：scrobble → `listen_type=single`（`listened_at`），
//!   now_playing → `listen_type=playing_now`（进度毫秒）；`POST /1/submit-listens`
//!   带 `Authorization: Token <token>`。<30s 曲目直接接受跳过；未配置 token
//!   拒绝（不可重试）。
//! - **`authorize` 动作**：`GET /1/validate-token` 校验 token，成功 `ok:true`
//!   （附账户名），失败区分 token 无效与网络错。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tma_plugin_sdk::{
    DEFAULT_USER_AGENT, GuestHttp, PluginActionRequest, PluginActionResponse, PluginError,
    PluginErrorCode, PluginEventRequest, PluginEventResponse, load_config,
};

use crate::scrobble;

// FFI 样板（manifest 导出 / host_fn 声明 / scrape 分发 / tma_event 导出）由宏吐出。
tma_plugin_sdk::plugin! {
    manifest = "../manifest.json",
    slug = "listenbrainz",
    config = read_config,
    actions = run_action,
    event = handle_event,
}

const SUBMIT_URL: &str = "https://api.listenbrainz.org/1/submit-listens";
const VALIDATE_URL: &str = "https://api.listenbrainz.org/1/validate-token";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 运行时配置（全局/用户合并视图）：`{"token": secret}`。
#[derive(Deserialize, Default)]
#[serde(default)]
struct ListenBrainzConfig {
    token: String,
}

// ---------------------------------------------------------------------------
// 出站辅助
// ---------------------------------------------------------------------------

/// 认证头（官方约定：`Authorization: Token <token>`，无 Bearer 前缀）。
fn auth_headers(token: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Token {token}"));
    headers
}

/// body → JSON（空/纯空白 → Null，与 `GuestHttp` 口径一致）。
fn parse_body_json(method: &str, url: &str, raw: &[u8]) -> Result<Value, PluginError> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(raw).map_err(|e| {
        PluginError::new(
            PluginErrorCode::Internal,
            format!("响应不是合法 JSON（{method} {url}）: {e}"),
        )
    })
}

// ---------------------------------------------------------------------------
// 动作：authorize（token 校验，message 按 locale 本地化）
// ---------------------------------------------------------------------------

fn run_action(req: PluginActionRequest) -> Result<PluginActionResponse, PluginError> {
    let zh = req.locale.as_deref().is_some_and(|l| l.starts_with("zh"));
    if req.action_id != "authorize" {
        return Ok(PluginActionResponse::failure(if zh {
            format!("未知动作：{}", req.action_id)
        } else {
            format!("unknown action: {}", req.action_id)
        }));
    }
    let token = load_config::<ListenBrainzConfig>(&read_config()?)
        .token
        .trim()
        .to_string();
    if token.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "未配置 token，请先在用户配置中填写 ListenBrainz 用户 token".into()
        } else {
            "token is not configured; set your ListenBrainz user token in the user config first"
                .into()
        }));
    }
    let mut headers = auth_headers(&token);
    headers.insert("Accept".to_string(), "application/json".to_string());
    // request_raw 不做状态分类：401（token 无效）与网络错的文案区分在调用点。
    let raw = GuestHttp::request_raw(&transport, "GET", VALIDATE_URL, headers, None)?;
    if !(200..300).contains(&raw.status) {
        let err = scrobble::status_error(raw.status, &raw.body_text_lossy());
        let token_invalid = matches!(raw.status, 401);
        return Ok(PluginActionResponse::failure(if token_invalid {
            if zh {
                format!("授权失败：token 无效（{}）", err.message)
            } else {
                format!("Authorization failed: invalid token ({})", err.message)
            }
        } else if matches!(
            err.code,
            PluginErrorCode::Network | PluginErrorCode::RateLimited
        ) {
            if zh {
                format!("授权失败：网络错误或服务暂不可用（{}）", err.message)
            } else {
                format!(
                    "Authorization failed: network error or service unavailable ({})",
                    err.message
                )
            }
        } else {
            err.message
        }));
    }
    let body = parse_body_json("GET", VALIDATE_URL, &raw.body)?;
    // 200 但 valid!=true（协议防御：正常无效 token 走 401，这里兜底）。
    if body
        .get("valid")
        .is_some_and(|v| !v.as_bool().unwrap_or(false))
    {
        return Ok(PluginActionResponse::failure(if zh {
            "授权失败：token 无效".into()
        } else {
            "Authorization failed: invalid token".into()
        }));
    }
    let user = body
        .get("user_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(PluginActionResponse::success(match user {
        Some(name) if zh => format!("已授权（ListenBrainz 用户：{name}）"),
        Some(name) => format!("Authorized (ListenBrainz user: {name})"),
        None if zh => "已授权".into(),
        None => "Authorized".into(),
    }))
}

// ---------------------------------------------------------------------------
// 事件：scrobble / now_playing 上报
// ---------------------------------------------------------------------------

/// `tma_event` 处理器：未配置 token → 拒绝（不可重试）；<30s 曲目直接接受
/// （跳过不计失败）；其余经 [`scrobble::build_submit_body`] 组装载荷提交。
fn handle_event(req: PluginEventRequest) -> Result<PluginEventResponse, PluginError> {
    let cfg = load_config::<ListenBrainzConfig>(&read_config()?);
    let token = cfg.token.trim().to_string();
    if token.is_empty() {
        return Ok(PluginEventResponse::rejected(PluginError::new(
            PluginErrorCode::InvalidArgument,
            "ListenBrainz 未配置 token，请先在用户配置中填写用户 token".into(),
        )));
    }
    let kind = match &req {
        PluginEventRequest::Scrobble(payload) => {
            if scrobble::should_skip(payload.track.duration_seconds) {
                return Ok(PluginEventResponse::accepted());
            }
            scrobble::ListenSubmit::Single {
                listened_at: payload.played_at,
            }
        }
        PluginEventRequest::NowPlaying(payload) => scrobble::ListenSubmit::PlayingNow {
            position_ms: payload.position_ms,
            duration_ms: payload.duration_ms,
        },
    };
    let body = scrobble::build_submit_body(&kind, req.track());
    let mut headers = auth_headers(&token);
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    let raw = GuestHttp::request_raw(
        &transport,
        "POST",
        SUBMIT_URL,
        headers,
        Some(body.as_bytes()),
    )?;
    if !(200..300).contains(&raw.status) {
        // 401 token 无效 / 400 载荷被拒 / 429 限流 / 5xx 网络的映射见码表。
        return Ok(PluginEventResponse::rejected(scrobble::status_error(
            raw.status,
            &raw.body_text_lossy(),
        )));
    }
    Ok(PluginEventResponse::accepted())
}
