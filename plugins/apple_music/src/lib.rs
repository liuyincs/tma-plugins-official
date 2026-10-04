//! 内置来源插件 `apple_music`：目录搜索持久化艺术家 Apple ID（需 developer token）。
//!
//! 移植自原生 `crates/scrape/src/providers/apple_music.rs`（该文件即规格）：
//! `GET /v1/catalog/{storefront}/search?term=...&types=artists&limit=1`，
//! `Authorization: Bearer <developer_token>`；已知 Apple ID 直接复用不再外呼。
//! 落库 provider 字面量为 `apple_music`。

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
    load_config, percent_encode_query, truncate_hint,
};

// FFI 样板（manifest 导出 / host_fn / scrape 分发 / transport / read_config）由宏吐出。
tma_plugin_sdk::scrape_plugin! {
    manifest = "../manifest.json",
    slug = "apple_music",
    scrape = run_scrape,
    config = read_config,
    actions = run_action,
}

const APPLE_CATALOG: &str = "https://api.music.apple.com/v1/catalog";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 运行时配置：`{"developer_token": secret, "storefront": "us"}`。
#[derive(Deserialize, Default)]
#[serde(default)]
struct AppleConfig {
    developer_token: String,
    storefront: String,
}

/// 与原生 `credentials::normalize_storefront` 一致：trim 后空串回退 `us`。
fn normalize_storefront(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "us".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// 动作：test_connection（轻量探测，message 按 locale 本地化）
// ---------------------------------------------------------------------------

/// `test_connection`：与刮削同端点的目录搜索 `limit=1`（storefront 读配置，
/// 缺省 us）；`request_raw` 直看状态——401 = developer token 无效/过期（ok=false
/// 并指明），2xx = 凭据有效。未配置 developer_token 时 ok=false 并说明。
fn run_action(req: PluginActionRequest) -> Result<PluginActionResponse, PluginError> {
    let zh = req.locale.as_deref().is_some_and(|l| l.starts_with("zh"));
    if req.action_id != "test_connection" {
        return Ok(PluginActionResponse::failure(if zh {
            format!("未知动作：{}", req.action_id)
        } else {
            format!("unknown action: {}", req.action_id)
        }));
    }
    let cfg = load_config::<AppleConfig>(&read_config()?);
    let dev_token = cfg.developer_token.trim().to_string();
    if dev_token.is_empty() {
        return Ok(PluginActionResponse::failure(if zh {
            "未配置 developer token，请先在插件配置中填写".into()
        } else {
            "developer_token is not configured; set it in the plugin config first".into()
        }));
    }
    let storefront = normalize_storefront(&cfg.storefront);
    let url = format!(
        "{APPLE_CATALOG}/{storefront}/search?term={}&types=artists&limit=1",
        percent_encode_query("abba")
    );
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {dev_token}"));
    let raw = GuestHttp::request_raw(&transport, "GET", &url, headers, None)?;
    if (200..300).contains(&raw.status) {
        return Ok(PluginActionResponse::success(if zh {
            "连接成功，developer token 有效".into()
        } else {
            "Connection OK, the developer token is valid".into()
        }));
    }
    let mut resp = if raw.status == 401 {
        PluginActionResponse::failure(if zh {
            "连接失败：developer token 无效或已过期（HTTP 401）".into()
        } else {
            "Connection failed: the developer token is invalid or expired (HTTP 401)".into()
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
// 刮削主流程（对应原生 AppleMusicAdapter::fetch，仅 Artist）
// ---------------------------------------------------------------------------

fn run_scrape(query: EntityQuery) -> Result<ScrapeResult, PluginError> {
    if query.kind != ScrapeEntityKind::Artist {
        return Ok(ScrapeResult::default());
    }
    let cfg = load_config::<AppleConfig>(&read_config()?);
    let dev_token = cfg.developer_token.trim().to_string();
    // 原生 `AppleMusicAdapter::new` 空 token 返回 None（来源不注册）；插件侧等价为永久失败。
    if dev_token.is_empty() {
        return Err(PluginError::new(
            PluginErrorCode::PermanentFailure,
            "Apple Music 未配置 developer_token".into(),
        ));
    }
    let storefront = normalize_storefront(&cfg.storefront);

    // 优先复用已知 Apple ID（无需再查）。
    if let Some(id) = known_id(&query, "apple_music") {
        return Ok(ScrapeResult {
            external_ids: vec![FetchedId {
                provider: "apple_music".into(),
                external_id: id.to_string(),
                url: None,
            }],
            confidence: 1.0,
            ..Default::default()
        });
    }
    let Some(name) = query.name.as_deref() else {
        return Ok(ScrapeResult::default());
    };
    let url = format!(
        "{APPLE_CATALOG}/{storefront}/search?term={}&types=artists&limit=1",
        percent_encode_query(name)
    );
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {dev_token}"));
    let body = GuestHttp::get_json(&transport, &url, headers)?;
    let id = parse_apple_search(&body);
    let confidence = if id.is_some() { 0.9 } else { 0.0 };
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

/// 解析 Apple catalog 搜索 JSON → 首个 artist 的 Apple ID。
fn parse_apple_search(body: &Value) -> Option<FetchedId> {
    let artist = body
        .get("results")
        .and_then(|r| r.get("artists"))
        .and_then(|a| a.get("data"))
        .and_then(|d| d.as_array())
        .and_then(|arr| arr.first())?;
    let id = artist.get("id").and_then(|v| v.as_str())?.to_string();
    let url = artist
        .get("attributes")
        .and_then(|at| at.get("url"))
        .and_then(|u| u.as_str())
        .map(str::to_string);
    Some(FetchedId {
        provider: "apple_music".into(),
        external_id: id,
        url,
    })
}
