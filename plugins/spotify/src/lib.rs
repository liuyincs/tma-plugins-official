//! 内置来源插件 `spotify`：Client Credentials 检索艺术家 ID。
//!
//! 移植自原生 `crates/scrape/src/providers/spotify.rs`（该文件即规格）：
//! token 端点（Basic client credentials）→ `GET /v1/search`（Bearer），
//! ISRC 查 track、名称查 artist，只持久化 ID。落库 provider 字面量为 `spotify`。
//!
//! 与原生的两点结构性差异（wasm 引擎 instance-per-call，无跨调用状态）：
//!
//! - **token 不缓存**：每次调用都重走 Client Credentials（原生进程内缓存至过期）。
//!   token 端点配额按调用次数计，代价可接受；管线侧插件桶限速（2 rps）兜底。
//! - **401/403 不冷却**：原生把 401/403（大陆封锁/凭证错）标记 24h 禁用并当次返回
//!   空结果；插件侧无法持久化禁用标记，改为直接返回 `PermanentFailure`
//!   （地理封锁语义），由管线退避层接管重试节奏。
//!
//! 与原生一样需要直接看 status 才能对 401/403 特判，故出站走
//! `GuestHttp::request_raw`（不做状态分类），特判保留在调用点。

// 宿主 target（cargo build --workspace）下整 crate 关闭：extism-pdk 引用的宿主函数
// （alloc/free 等）只在 wasm target 存在，宿主编译必然链接失败。
// wasm 构建走 scripts/build-builtin-plugins.sh（--target wasm32-unknown-unknown）。
#![cfg(target_arch = "wasm32")]

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tma_plugin_sdk::{
    DEFAULT_USER_AGENT, EntityQuery, FetchedId, GuestHttp, PluginActionRequest,
    PluginActionResponse, PluginError, PluginErrorCode, ScrapeEntityKind, ScrapeResult,
    base64_encode, load_config, percent_encode_query, status_to_error, truncate_hint,
};

// FFI 样板（manifest 导出 / host_fn 声明 / scrape 分发 / transport / read_config）由宏吐出。
tma_plugin_sdk::scrape_plugin! {
    manifest = "../manifest.json",
    slug = "spotify",
    scrape = run_scrape,
    config = read_config,
    actions = run_action,
}

const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const SEARCH_URL: &str = "https://api.spotify.com/v1/search";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 运行时配置：`{"client_id": ..., "client_secret": secret}`。
#[derive(Deserialize, Default)]
#[serde(default)]
struct SpotifyConfig {
    client_id: String,
    client_secret: String,
}

// ---------------------------------------------------------------------------
// 出站辅助：request_raw 不做状态分类，401/403 特判在调用点（见模块注释）
// ---------------------------------------------------------------------------

/// body → JSON（空/纯空白 → Null，与 `GuestHttp` 口径一致；原生 `resp.json()` 对空体报错，
/// 此处以 Null 走「字段缺失」路径，更稳）。
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

/// 401/403 的统一投影（原生语义：大陆封锁/凭证错 → 24h 冷却；插件侧不可冷却，见模块注释）。
fn geo_restriction_error(status: u16) -> PluginError {
    PluginError::new(
        PluginErrorCode::PermanentFailure,
        format!(
            "Spotify HTTP {status}：疑似地理封锁或 client credentials 无效\
             （原生 24h 冷却在插件侧不可用，重试节奏交由管线退避处理）"
        ),
    )
}

// ---------------------------------------------------------------------------
// 动作：test_connection（轻量探测，message 按 locale 本地化）
// ---------------------------------------------------------------------------

/// `test_connection`：只走 Client Credentials 换 token（不发起后续搜索）；
/// 200 且拿到 `access_token` 即凭据有效，401/403 = 凭据无效或被拒。
/// 未配置 client id / secret 时 ok=false 并说明。
fn run_action(req: PluginActionRequest) -> Result<PluginActionResponse, PluginError> {
    let zh = req.locale.as_deref().is_some_and(|l| l.starts_with("zh"));
    if req.action_id != "test_connection" {
        return Ok(PluginActionResponse::failure(if zh {
            format!("未知动作：{}", req.action_id)
        } else {
            format!("unknown action: {}", req.action_id)
        }));
    }
    let cfg = load_config::<SpotifyConfig>(&read_config()?);
    let client_id = cfg.client_id.trim().to_string();
    let client_secret = cfg.client_secret.trim().to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "未配置 client id / client secret，请先在插件配置中填写".into()
        } else {
            "client_id/client_secret are not configured; set them in the plugin config first".into()
        }));
    }
    // 与 ensure_token 同构（此处要直看状态分类文案，不复用以免消息耦合刮削语义）。
    let basic = base64_encode(format!("{client_id}:{client_secret}").as_bytes());
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Basic {basic}"));
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    let raw = GuestHttp::request_raw(
        &transport,
        "POST",
        TOKEN_URL,
        headers,
        Some(b"grant_type=client_credentials"),
    )?;
    if (200..300).contains(&raw.status) {
        let body = parse_body_json("POST", TOKEN_URL, &raw.body)?;
        return if parse_token(&body).is_some() {
            Ok(PluginActionResponse::success(if zh {
                "连接成功，client credentials 有效".into()
            } else {
                "Connection OK, client credentials are valid".into()
            }))
        } else {
            Ok(PluginActionResponse::failure(if zh {
                "连接失败：token 响应缺少 access_token".into()
            } else {
                "Connection failed: token response has no access_token".into()
            }))
        };
    }
    let mut resp = if raw.status == 401 || raw.status == 403 {
        PluginActionResponse::failure(if zh {
            format!(
                "连接失败：client id/secret 无效或被拒（HTTP {}）",
                raw.status
            )
        } else {
            format!(
                "Connection failed: client id/secret rejected (HTTP {})",
                raw.status
            )
        })
    } else if zh {
        PluginActionResponse::failure(format!("连接失败：上游 HTTP {}", raw.status))
    } else {
        PluginActionResponse::failure(format!("Connection failed: upstream HTTP {}", raw.status))
    };
    let hint = truncate_hint(&raw.body_text_lossy());
    resp.details = if hint.is_empty() { None } else { Some(hint) };
    Ok(resp)
}

// ---------------------------------------------------------------------------
// 刮削主流程（对应原生 SpotifyAdapter::fetch，仅 Artist）
// ---------------------------------------------------------------------------

fn run_scrape(query: EntityQuery) -> Result<ScrapeResult, PluginError> {
    if query.kind != ScrapeEntityKind::Artist {
        return Ok(ScrapeResult::default());
    }
    let cfg = load_config::<SpotifyConfig>(&read_config()?);
    let client_id = cfg.client_id.trim().to_string();
    let client_secret = cfg.client_secret.trim().to_string();
    // 原生 `SpotifyAdapter::new` 任一为空返回 None（来源不注册）；插件侧等价为永久失败。
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(PluginError::new(
            PluginErrorCode::PermanentFailure,
            "Spotify client credentials not configured".into(),
        ));
    }

    // 优先复用已知 Spotify ID（无需再查）。
    if let Some(id) = known_id(&query, "spotify") {
        return Ok(ScrapeResult {
            external_ids: vec![FetchedId {
                provider: "spotify".into(),
                external_id: id.to_string(),
                url: None,
            }],
            confidence: 1.0,
            ..Default::default()
        });
    }

    // ISRC 可用 → 查 track（高置信）；否则名称查 artist。
    let (query_term, qtype, conf) = if let Some(isrc) = query.isrc.as_deref() {
        (format!("isrc:{isrc}"), "track", 0.9f32)
    } else if let Some(name) = query.name.as_deref() {
        (name.to_string(), "artist", 0.8f32)
    } else {
        return Ok(ScrapeResult::default());
    };

    let token = ensure_token(&client_id, &client_secret)?;

    let url = format!(
        "{SEARCH_URL}?q={}&type={qtype}&limit=1",
        percent_encode_query(&query_term)
    );
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    // request_raw 不做状态分类：401/403 与其他非 2xx 的分类保留在本调用点。
    let raw = GuestHttp::request_raw(&transport, "GET", &url, headers, None)?;
    if !(200..300).contains(&raw.status) {
        if raw.status == 401 || raw.status == 403 {
            // 原生此处返回空结果并冷却 24h；插件侧改为显式失败（见模块注释）。
            return Err(geo_restriction_error(raw.status));
        }
        return Err(status_to_error(raw.status, &raw.body_text_lossy()));
    }
    let body = parse_body_json("GET", &url, &raw.body)?;
    let id = parse_spotify_search(&body);
    let confidence = if id.is_some() { conf } else { 0.0 };
    Ok(ScrapeResult {
        external_ids: id.into_iter().collect(),
        confidence,
        ..Default::default()
    })
}

/// 在已知 ID 中查找指定来源的 external_id（对应原生 EntityQuery::known_id）。
fn known_id<'a>(q: &'a EntityQuery, provider: &str) -> Option<&'a str> {
    q.known_external_ids
        .iter()
        .find(|f| f.provider == provider)
        .map(|f| f.external_id.as_str())
}

/// 取 access_token（对应原生 ensure_token；无进程内缓存，每次调用换新，见模块注释）。
fn ensure_token(client_id: &str, client_secret: &str) -> Result<String, PluginError> {
    // 原生 reqwest `basic_auth` 的等价：Authorization: Basic base64(id:secret)（标准字母表）。
    let basic = base64_encode(format!("{client_id}:{client_secret}").as_bytes());
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Basic {basic}"));
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    let raw = GuestHttp::request_raw(
        &transport,
        "POST",
        TOKEN_URL,
        headers,
        Some(b"grant_type=client_credentials"),
    )?;
    if !(200..300).contains(&raw.status) {
        if raw.status == 401 || raw.status == 403 {
            return Err(geo_restriction_error(raw.status));
        }
        return Err(status_to_error(raw.status, &raw.body_text_lossy()));
    }
    let body = parse_body_json("POST", TOKEN_URL, &raw.body)?;
    parse_token(&body)
        .map(|(tok, _expires_in)| tok)
        .ok_or_else(|| {
            PluginError::new(PluginErrorCode::Internal, "Spotify token 响应缺字段".into())
        })
}

/// 解析 token 端点 JSON：`{ access_token, expires_in }`。
/// `expires_in` 在插件侧无处可用（instance-per-call，无缓存），仅保持解析形状一致。
fn parse_token(body: &Value) -> Option<(String, u64)> {
    let token = body
        .get("access_token")
        .and_then(|v| v.as_str())?
        .to_string();
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    Some((token, expires_in))
}

/// 解析搜索 JSON：优先 tracks.items[0].artists[0]，回退 artists.items[0]。
fn parse_spotify_search(body: &Value) -> Option<FetchedId> {
    // ISRC 路径：track → 其首个 artist。
    let from_track = body
        .get("tracks")
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .and_then(|a| a.first())
        .and_then(|tr| tr.get("artists"))
        .and_then(|ar| ar.as_array())
        .and_then(|a| a.first())
        .and_then(|artist| artist.get("id"))
        .and_then(|v| v.as_str());
    let id = from_track.or_else(|| {
        body.get("artists")
            .and_then(|a| a.get("items"))
            .and_then(|i| i.as_array())
            .and_then(|arr| arr.first())
            .and_then(|artist| artist.get("id"))
            .and_then(|v| v.as_str())
    })?;
    Some(FetchedId {
        provider: "spotify".into(),
        external_id: id.to_string(),
        url: Some(format!("https://open.spotify.com/artist/{id}")),
    })
}
