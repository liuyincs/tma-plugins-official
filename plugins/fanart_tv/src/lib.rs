//! 内置来源插件 `fanart`：艺术家多套图与专辑封面（需 api_key）。
//!
//! 移植自原生 `crates/scrape/src/providers/fanart.rs`（该文件即规格）：
//! 按 MBID 请求 `v3/music/{mbid}`（艺术家）与 `v3/music/albums/{mbid}`（专辑），
//! api_key 拼在 query；thumb 作主图，各 kind 取 likes 最高。落库 source/provider
//! 字面量为 `fanart`。无 MBID 不发 HTTP，直接返回空结果。

// 宿主 target（cargo build --workspace）下整 crate 关闭：extism-pdk 引用的宿主函数
// （alloc/free 等）只在 wasm target 存在，宿主编译必然链接失败。
// wasm 构建走 scripts/build-builtin-plugins.sh（--target wasm32-unknown-unknown）。
#![cfg(target_arch = "wasm32")]

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tma_plugin_sdk::normalize_mbid;
use tma_plugin_sdk::{
    DEFAULT_USER_AGENT, EntityQuery, FetchedId, GuestHttp, PluginActionRequest,
    PluginActionResponse, PluginError, PluginErrorCode, RemoteExtraImage, RemoteImage,
    ScrapeEntityKind, ScrapeResult, load_config,
};

// FFI 样板（manifest 导出 / host_fn 声明 / scrape 分发 / transport / read_config）由宏吐出。
tma_plugin_sdk::scrape_plugin! {
    manifest = "../manifest.json",
    slug = "fanart",
    scrape = run_scrape,
    config = read_config,
    actions = run_action,
}

const FANART_API: &str = "https://webservice.fanart.tv/v3/music";

// ---------------------------------------------------------------------------
// 配置与出站
// ---------------------------------------------------------------------------

/// 运行时配置：`{"api_key": secret}`。
#[derive(Deserialize, Default)]
#[serde(default)]
struct FanartConfig {
    api_key: String,
}

fn fanart_get(url: &str) -> Result<Value, PluginError> {
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    GuestHttp::get_json(&transport, url, headers)
}

// ---------------------------------------------------------------------------
// 动作：test_connection（轻量探测，message 按 locale 本地化）
// ---------------------------------------------------------------------------

/// 探针实体：The Beatles 的 MBID（固定知名艺人，music API 一次轻查询）。
const PROBE_MBID: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d";

/// `test_connection`：music API 按 MBID 轻查询；Fanart.tv 以 200 + body 级
/// `status:"error"`（如 "Bad API key"）表达失败，需在 body 层判别。
/// 未配置 api_key 时 ok=false 并说明。
fn run_action(req: PluginActionRequest) -> Result<PluginActionResponse, PluginError> {
    let zh = req.locale.as_deref().is_some_and(|l| l.starts_with("zh"));
    if req.action_id != "test_connection" {
        return Ok(PluginActionResponse::failure(if zh {
            format!("未知动作：{}", req.action_id)
        } else {
            format!("unknown action: {}", req.action_id)
        }));
    }
    let api_key = load_config::<FanartConfig>(&read_config()?)
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
    let url = format!("{FANART_API}/{PROBE_MBID}?api_key={api_key}");
    match fanart_get(&url) {
        Ok(body) => {
            if body.get("status").and_then(|v| v.as_str()) == Some("error") {
                let detail = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                return Ok(PluginActionResponse::failure(if zh {
                    format!("连接失败：Fanart.tv 返回错误（{detail}）")
                } else {
                    format!("Connection failed: Fanart.tv returned an error ({detail})")
                }));
            }
            Ok(PluginActionResponse::success(if zh {
                "连接成功，凭据有效".into()
            } else {
                "Connection OK, credentials are valid".into()
            }))
        }
        Err(e) => Ok(PluginActionResponse::failure(e.message)),
    }
}

// ---------------------------------------------------------------------------
// 刮削主流程（对应原生 FanartTvAdapter::fetch_artist / fetch_album）
// ---------------------------------------------------------------------------

fn run_scrape(query: EntityQuery) -> Result<ScrapeResult, PluginError> {
    let cfg = load_config::<FanartConfig>(&read_config()?);
    let api_key = cfg.api_key.trim().to_string();
    // 原生 `FanartTvAdapter::new` 空 key 返回 None（来源不注册）；插件侧等价为永久失败。
    if api_key.is_empty() {
        return Err(PluginError::new(
            PluginErrorCode::PermanentFailure,
            "Fanart.tv 未配置 api_key".into(),
        ));
    }

    match query.kind {
        ScrapeEntityKind::Artist => fetch_artist(&query, &api_key),
        ScrapeEntityKind::Album => fetch_album(&query, &api_key),
        ScrapeEntityKind::Track => Ok(ScrapeResult::default()),
    }
}

/// 从 query.mbid 或已知 musicbrainz ID 解析 MBID（对应原生 resolve_mbid）。
fn resolve_mbid(q: &EntityQuery) -> Option<String> {
    q.mbid
        .as_deref()
        .and_then(normalize_mbid)
        .or_else(|| known_id(q, "musicbrainz").and_then(normalize_mbid))
}

/// 在已知 ID 中查找指定来源的 external_id（对应原生 EntityQuery::known_id）。
fn known_id<'a>(q: &'a EntityQuery, provider: &str) -> Option<&'a str> {
    q.known_external_ids
        .iter()
        .find(|f| f.provider == provider)
        .map(|f| f.external_id.as_str())
}

fn fetch_artist(q: &EntityQuery, api_key: &str) -> Result<ScrapeResult, PluginError> {
    let Some(mbid) = resolve_mbid(q) else {
        return Ok(ScrapeResult::default());
    };
    // 与原生一致的 URL 构造：api_key 原样拼在 query（不做额外编码）。
    let url = format!("{FANART_API}/{mbid}?api_key={api_key}");
    let body = fanart_get(&url)?;
    let parsed = parse_fanart_response(&body);
    let confidence = if parsed.image.is_some() || !parsed.extra_images.is_empty() {
        1.0
    } else {
        0.0
    };
    let external_ids = vec![FetchedId {
        provider: "fanart".into(),
        external_id: mbid.clone(),
        url: Some(format!("https://fanart.tv/artist/{mbid}")),
    }];
    Ok(ScrapeResult {
        external_ids,
        image: parsed.image,
        extra_images: parsed.extra_images,
        confidence,
        ..Default::default()
    })
}

fn fetch_album(q: &EntityQuery, api_key: &str) -> Result<ScrapeResult, PluginError> {
    let Some(mbid) = resolve_mbid(q) else {
        return Ok(ScrapeResult::default());
    };
    let url = format!("{FANART_API}/albums/{mbid}?api_key={api_key}");
    let body = fanart_get(&url)?;
    let image = parse_fanart_album_cover(&body);
    let confidence = if image.is_some() { 1.0 } else { 0.0 };
    let external_ids = vec![FetchedId {
        provider: "fanart".into(),
        external_id: mbid.clone(),
        url: Some(format!("https://fanart.tv/music/{mbid}")),
    }];
    Ok(ScrapeResult {
        external_ids,
        image,
        confidence,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// 响应解析（与原生逐字对应）
// ---------------------------------------------------------------------------

struct ParsedFanart {
    image: Option<RemoteImage>,
    extra_images: Vec<RemoteExtraImage>,
}

/// 解析 Fanart.tv JSON：thumb 作主图，各 kind 取 likes 最高。
fn parse_fanart_response(body: &Value) -> ParsedFanart {
    // (已解析的最佳 url, extra_image kind) 配置表，按输出顺序排列。
    // thumb 在构建时直接捕获为主图（不靠事后扫描 extra_images）。
    // musiclogo：hdmusiclogo 优先、缺失回退 musiclogo，kind 统一 musiclogo。
    let candidates: [(Option<String>, &str); 4] = [
        (pick_best_url(body.get("artistthumb")), "thumb"),
        (
            pick_best_url(body.get("artistbackground")),
            "artistbackground",
        ),
        (
            pick_best_url(body.get("hdmusiclogo")).or_else(|| pick_best_url(body.get("musiclogo"))),
            "musiclogo",
        ),
        (pick_best_url(body.get("musicbanner")), "musicbanner"),
    ];

    let mut extra_images = Vec::new();
    let mut thumb_url: Option<String> = None;
    for (url_opt, kind) in candidates {
        let Some(url) = url_opt else { continue };
        if kind == "thumb" {
            thumb_url = Some(url.clone());
        }
        extra_images.push(RemoteExtraImage {
            kind: kind.into(),
            url,
            source: "fanart".into(),
        });
    }

    let image = thumb_url.map(|url| RemoteImage {
        url,
        license: Some("Fanart.tv".into()),
        attribution: None,
        source: "fanart".into(),
    });

    ParsedFanart {
        image,
        extra_images,
    }
}

/// 解析 Fanart.tv 专辑 JSON：`albums` 对象内 `albumcover`（likes 最高）。
fn parse_fanart_album_cover(body: &Value) -> Option<RemoteImage> {
    let albums = body.get("albums")?;
    let entries: Vec<&Value> = if albums.is_object() {
        albums.as_object()?.values().collect()
    } else if albums.is_array() {
        albums.as_array()?.iter().collect()
    } else {
        return None;
    };
    let mut best_url: Option<String> = None;
    for entry in entries {
        if let Some(url) = pick_best_url(entry.get("albumcover")) {
            best_url = Some(url);
            break;
        }
    }
    best_url.map(|url| RemoteImage {
        url,
        license: Some("Fanart.tv".into()),
        attribution: None,
        source: "fanart".into(),
    })
}

/// 数组中取 likes 最高项的 url；单对象也支持。
fn pick_best_url(node: Option<&Value>) -> Option<String> {
    let items: Vec<&Value> = match node {
        Some(v) if v.is_array() => v.as_array()?.iter().collect(),
        Some(v) => vec![v],
        None => return None,
    };
    let mut best: Option<(i64, String)> = None;
    for item in items {
        let Some(url) = item.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let likes = item
            .get("likes")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| item.get("likes").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        if best.as_ref().is_none_or(|(b, _)| likes > *b) {
            best = Some((likes, url.to_string()));
        }
    }
    best.map(|(_, url)| url)
}
