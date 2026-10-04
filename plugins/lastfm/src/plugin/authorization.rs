//! Last.fm connection checking and browser authorization lifecycle.

use super::*;
use tma_plugin_sdk::{PluginActionRequest, PluginActionResponse};

pub(crate) fn run_action(req: PluginActionRequest) -> Result<PluginActionResponse, PluginError> {
    let zh = req.locale.as_deref().is_some_and(|l| l.starts_with("zh"));
    match req.action_id.as_str() {
        "test_connection" => run_test_connection(zh),
        "authorize" => run_authorize(zh, req.payload.as_ref()),
        other => Ok(PluginActionResponse::failure(if zh {
            format!("未知动作：{other}")
        } else {
            format!("unknown action: {other}")
        })),
    }
}

fn run_test_connection(zh: bool) -> Result<PluginActionResponse, PluginError> {
    let api_key = load_config::<LastFmConfig>(&read_config()?)
        .api_key
        .trim()
        .to_string();
    if api_key.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "未配置 api_key，请先在插件配置中填写".into()
        } else {
            "api_key is not configured; set it in the plugin config first".into()
        }));
    }
    let params: [(&str, &str); 4] = [
        ("method", "chart.gettopartists"),
        ("api_key", &api_key),
        ("format", "json"),
        ("limit", "1"),
    ];
    match lastfm_get(&params) {
        Ok(_) => Ok(PluginActionResponse::success(if zh {
            "连接成功，凭据有效".into()
        } else {
            "Connection OK, credentials are valid".into()
        })),
        Err(e) => Ok(PluginActionResponse::failure(e.message)),
    }
}

fn run_authorize(zh: bool, payload: Option<&Value>) -> Result<PluginActionResponse, PluginError> {
    let cfg = load_config::<LastFmConfig>(&read_config()?);
    let api_key = cfg.api_key.trim().to_string();
    let shared_secret = cfg.shared_secret.trim().to_string();
    if api_key.is_empty() || shared_secret.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "管理员尚未配置 Last.fm 的 API key 与 Shared secret，无法连接".into()
        } else {
            "Last.fm API key and shared secret are not configured by an admin".into()
        }));
    }
    let phase = payload
        .and_then(|value| value.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("start");
    match phase {
        "complete" => complete_web_auth(zh, payload, &api_key, &shared_secret),
        _ => start_web_auth(zh, &api_key, &shared_secret),
    }
}

fn start_web_auth(
    zh: bool,
    api_key: &str,
    shared_secret: &str,
) -> Result<PluginActionResponse, PluginError> {
    let body = scrobble::build_get_token_body(api_key, shared_secret);
    let resp = match lastfm_post_signed(&body) {
        Ok(value) => value,
        Err(e) => {
            return Ok(PluginActionResponse::failure(authorize_transport_message(
                zh, &e,
            )));
        }
    };
    if let Some(err) = scrobble::body_error(&resp) {
        return Ok(PluginActionResponse::failure(authorize_upstream_message(
            zh, &err,
        )));
    }
    let Some(token) = resp
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Ok(PluginActionResponse::failure(if zh {
            "授权失败：响应缺少 token".into()
        } else {
            "Authorization failed: response has no token".into()
        }));
    };
    Ok(PluginActionResponse::pending_authorization(
        if zh {
            "请在打开的 Last.fm 页面允许访问".into()
        } else {
            "Allow access in the Last.fm tab that just opened".into()
        },
        scrobble::authorization_url(api_key, &token),
        token,
    ))
}

fn complete_web_auth(
    zh: bool,
    payload: Option<&Value>,
    api_key: &str,
    shared_secret: &str,
) -> Result<PluginActionResponse, PluginError> {
    let token = payload
        .and_then(|value| value.get("token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if token.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "授权未开始：请先点击连接".into()
        } else {
            "Authorization has not started; click Connect first".into()
        }));
    }
    let body = scrobble::build_get_session_body(token, api_key, shared_secret);
    let resp = match lastfm_post_signed(&body) {
        Ok(value) => value,
        Err(e) => {
            return Ok(PluginActionResponse::failure(authorize_transport_message(
                zh, &e,
            )));
        }
    };
    if scrobble::is_token_not_authorized(&resp) {
        return Ok(PluginActionResponse::pending_wait(if zh {
            "等待 Last.fm 授权".into()
        } else {
            "Waiting for Last.fm authorization".into()
        }));
    }
    if let Some(err) = scrobble::body_error(&resp) {
        return Ok(PluginActionResponse::failure(authorize_upstream_message(
            zh, &err,
        )));
    }
    let Some(session_key) = resp
        .get("session")
        .and_then(|s| s.get("key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Ok(PluginActionResponse::failure(if zh {
            "授权失败：响应缺少 session.key".into()
        } else {
            "Authorization failed: response has no session.key".into()
        }));
    };
    let account = resp
        .get("session")
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let details = account.map(|name| {
        if zh {
            format!("Last.fm 账户：{name}")
        } else {
            format!("Last.fm account: {name}")
        }
    });
    let mut persist = BTreeMap::new();
    persist.insert("session_key".to_string(), session_key);
    Ok(PluginActionResponse {
        ok: true,
        message: if zh {
            "已授权".into()
        } else {
            "Authorized".into()
        },
        details,
        persist_secrets: Some(persist),
        authorization_url: None,
        resume_token: None,
        pending: false,
    })
}

fn authorize_transport_message(zh: bool, err: &PluginError) -> String {
    let networkish = matches!(
        err.code,
        PluginErrorCode::Network | PluginErrorCode::RateLimited
    );
    if networkish {
        if zh {
            format!("授权失败：网络错误或服务暂不可用（{}）", err.message)
        } else {
            format!(
                "Authorization failed: network error or service unavailable ({})",
                err.message
            )
        }
    } else if zh {
        format!("授权失败：请求被拒绝（{}）", err.message)
    } else {
        format!("Authorization failed: request rejected ({})", err.message)
    }
}

fn authorize_upstream_message(zh: bool, err: &PluginError) -> String {
    if matches!(err.code, PluginErrorCode::InvalidArgument) {
        if zh {
            format!("授权失败：API key 或 Shared secret 无效（{}）", err.message)
        } else {
            format!(
                "Authorization failed: invalid API key or shared secret ({})",
                err.message
            )
        }
    } else if zh {
        format!("授权失败：{}", err.message)
    } else {
        format!("Authorization failed: {}", err.message)
    }
}
