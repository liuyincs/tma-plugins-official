//! lastfm 插件的 wasm 本体（仅 wasm target 编译，见 `lib.rs` 模块布局注释）。
//!
//! 刮削实现移植自原生 `crates/scrape/src/providers/lastfm.rs`（该文件即规格）：
//! artist.getInfo / album.getInfo 与 artist.search / album.search 共 4 个方法，
//! api_key 走 query 参数，bio 空时回退英文重取。清单 slug 与落库 provider/source
//! 字面量统一为 `lastfm`。
//!
//! 0.3.0 起新增两块业务（纯逻辑在 `crate::scrobble`，双 target 编译可单测）：
//!
//! - **`authorize` 动作**：网页授权。`start` 调 `auth.getToken`，回
//!   `authorization_url` + `resume_token`；用户在 Last.fm 点允许后，前端把
//!   token 带回 `complete`，插件调 `auth.getSession` 换长期 session key，
//!   经 `persist_secrets` 交宿主落盘。不收集 Last.fm 用户名/密码。
//!   API key / Shared secret 由管理员配在全局配置。
//! - **`tma_event` 处理器**：scrobble → `track.scrobble`（`played_at` 作 timestamp），
//!   now_playing → `track.updateNowPlaying`（无时间戳）；<30s 曲目直接接受跳过；
//!   会话失效（error 9）需用户到「我的集成」重新连接。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tma_plugin_sdk::{
    Bio, DEFAULT_USER_AGENT, EntityQuery, FetchedId, GuestHttp, PluginError, PluginErrorCode,
    PluginEventRequest, PluginEventResponse, RemoteImage, ScrapeEntityKind, ScrapeResult,
    load_config, percent_encode_query, status_to_error,
};
use tma_plugin_sdk::{name_similarity, normalize_mbid};

use crate::scrobble;

mod authorization;

// FFI 样板（manifest 导出 / host_fn 声明 / scrape 分发 / tma_event 导出）由宏吐出。
tma_plugin_sdk::scrape_plugin! {
    manifest = "../manifest.json",
    slug = "lastfm",
    scrape = run_scrape,
    config = read_config,
    actions = authorization::run_action,
    event = handle_event,
}

const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0/";

/// 与原生 `tma_scrape::matching::CONFIDENCE_THRESHOLD` 一致（搜索匹配阈值）。
const CONFIDENCE_THRESHOLD: f32 = 0.75;

/// Last.fm 未上传图时的占位星标（各尺寸 URL 都带此 hash）。
const LASTFM_PLACEHOLDER_HASH: &str = "2a96cbd8b46e442fc41c2b86b821562f";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 运行时配置：刮削侧 `{"api_key": secret, "language": "zh"|"en"|...}`
/// （全局配置；用户级若仍残留旧字段会覆盖）。上报另需全局 `shared_secret`。
/// `session_key` 由网页授权 complete 经 persist_secrets 落盘，不作为用户填写项。
#[derive(Deserialize, Default)]
#[serde(default)]
struct LastFmConfig {
    api_key: String,
    language: String,
    shared_secret: String,
    session_key: String,
}

/// 与原生 `credentials::normalize_lastfm_language` 一致：闭合枚举外回退 `zh`。
fn normalize_lastfm_language(raw: &str) -> String {
    match raw.trim() {
        "zh" | "en" | "ja" | "de" | "fr" => raw.trim().to_string(),
        _ => "zh".to_string(),
    }
}

// ---------------------------------------------------------------------------
// 出站请求
// ---------------------------------------------------------------------------

/// query 组件串接：与原生 reqwest `.query(params)` 等价（空格编码为 %20 而非 +，
/// 服务端解码等价）。
fn build_query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode_query(k), percent_encode_query(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 发送 Last.fm 请求并校验 body 级 error（对应原生 lastfm_get：send_json + parse_api_error）。
fn lastfm_get(params: &[(&str, &str)]) -> Result<Value, PluginError> {
    let url = format!("{LASTFM_API}?{}", build_query(params));
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    let body = GuestHttp::get_json(&transport, &url, headers)?;
    match parse_api_error(&body) {
        Some(err) => Err(PluginError::new(PluginErrorCode::PermanentFailure, err)),
        None => Ok(body),
    }
}

/// POST 已签名 form body（scrobble / authorize 共用）：非 2xx 走共享状态码
/// 分类（429→限流、5xx→network 可重试、其余 4xx→permanent），2xx 再看 body 级
/// error（码表映射在 [`scrobble::body_error`]，与刮削侧任何 error 即 permanent
/// 的粗口径不同——上报错误需要区分可否重试）。
fn lastfm_post_signed(body: &str) -> Result<Value, PluginError> {
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), DEFAULT_USER_AGENT.to_string());
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    let raw = GuestHttp::request_raw(
        &transport,
        "POST",
        LASTFM_API,
        headers,
        Some(body.as_bytes()),
    )?;
    if !(200..300).contains(&raw.status) {
        return Err(status_to_error(raw.status, &raw.body_text_lossy()));
    }
    parse_body_json("POST", LASTFM_API, &raw.body)
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
// 事件：scrobble / now_playing 上报（0.3.0 起）
// ---------------------------------------------------------------------------

/// `tma_event` 处理器：无 session_key → 拒绝；<30s 曲目直接接受；
/// 上报遇 Last.fm error 9（会话失效）→ 拒绝，提示用户到「我的集成」重新连接。
fn handle_event(req: PluginEventRequest) -> Result<PluginEventResponse, PluginError> {
    let cfg = load_config::<LastFmConfig>(&read_config()?);
    let session_key = cfg.session_key.trim().to_string();
    if session_key.is_empty() {
        return Ok(PluginEventResponse::rejected(PluginError::new(
            PluginErrorCode::InvalidArgument,
            "Last.fm 未绑定：请到「我的集成」连接 Last.fm 账户".into(),
        )));
    }
    let api_key = cfg.api_key.trim().to_string();
    let shared_secret = cfg.shared_secret.trim().to_string();
    if api_key.is_empty() || shared_secret.is_empty() {
        return Ok(PluginEventResponse::rejected(PluginError::new(
            PluginErrorCode::InvalidArgument,
            "Last.fm 未配置 api_key/shared_secret，无法上报".into(),
        )));
    }

    let (method, track, timestamp) = match req {
        PluginEventRequest::Scrobble(payload) => {
            ("track.scrobble", payload.track, Some(payload.played_at))
        }
        PluginEventRequest::NowPlaying(payload) => ("track.updateNowPlaying", payload.track, None),
    };
    if scrobble::should_skip(track.duration_seconds) {
        return Ok(PluginEventResponse::accepted());
    }

    match submit_signed_track(
        method,
        &track,
        timestamp,
        &api_key,
        &session_key,
        &shared_secret,
    ) {
        Ok(()) => Ok(PluginEventResponse::accepted()),
        Err(err) if scrobble::is_invalid_session(&err) => {
            Ok(PluginEventResponse::rejected(PluginError::new(
                PluginErrorCode::InvalidArgument,
                "Last.fm 会话已失效，请到「我的集成」重新连接".into(),
            )))
        }
        Err(err) => Ok(PluginEventResponse::rejected(err)),
    }
}

fn submit_signed_track(
    method: &str,
    track: &tma_plugin_sdk::TrackEventFields,
    timestamp: Option<i64>,
    api_key: &str,
    session_key: &str,
    shared_secret: &str,
) -> Result<(), PluginError> {
    let body = scrobble::build_scrobble_body(
        method,
        track,
        timestamp,
        api_key,
        session_key,
        shared_secret,
    );
    let resp = lastfm_post_signed(&body)?;
    if let Some(err) = scrobble::body_error(&resp) {
        return Err(err);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 刮削主流程（对应原生 LastFmAdapter::fetch_artist / fetch_album）
// ---------------------------------------------------------------------------

fn run_scrape(query: EntityQuery) -> Result<ScrapeResult, PluginError> {
    let cfg = load_config::<LastFmConfig>(&read_config()?);
    let api_key = cfg.api_key.trim().to_string();
    // 原生 `LastFmAdapter::new` 空 key 返回 None（来源不注册）；插件侧等价为永久失败。
    if api_key.is_empty() {
        return Err(PluginError::new(
            PluginErrorCode::PermanentFailure,
            "Last.fm 未配置 api_key".into(),
        ));
    }
    let language = normalize_lastfm_language(&cfg.language);

    match query.kind {
        ScrapeEntityKind::Artist => fetch_artist(&query, &api_key, &language),
        ScrapeEntityKind::Album => fetch_album(&query, &api_key, &language),
        ScrapeEntityKind::Track => Ok(ScrapeResult::default()),
    }
}

fn get_info(
    api_key: &str,
    mbid: Option<&str>,
    artist: Option<&str>,
    lang: &str,
) -> Result<Value, PluginError> {
    let mut params: Vec<(&str, &str)> = vec![
        ("method", "artist.getInfo"),
        ("api_key", api_key),
        ("format", "json"),
        ("lang", lang),
    ];
    if let Some(m) = mbid {
        params.push(("mbid", m));
    } else if let Some(a) = artist {
        params.push(("artist", a));
    } else {
        return Err(PluginError::new(
            PluginErrorCode::InvalidArgument,
            "Last.fm getInfo 需 mbid 或 artist".into(),
        ));
    }
    lastfm_get(&params)
}

fn search_best_mbid(api_key: &str, name: &str) -> Result<Option<(String, f32)>, PluginError> {
    let params: [(&str, &str); 5] = [
        ("method", "artist.search"),
        ("artist", name),
        ("api_key", api_key),
        ("format", "json"),
        ("limit", "5"),
    ];
    let body = lastfm_get(&params)?;
    Ok(pick_best_search_match(&body, name))
}

fn album_get_info(
    api_key: &str,
    artist: &str,
    album: &str,
    lang: &str,
) -> Result<Value, PluginError> {
    let params: [(&str, &str); 6] = [
        ("method", "album.getInfo"),
        ("artist", artist),
        ("album", album),
        ("api_key", api_key),
        ("format", "json"),
        ("lang", lang),
    ];
    lastfm_get(&params)
}

fn search_best_album(
    api_key: &str,
    album: &str,
    artist: Option<&str>,
) -> Result<Option<(String, String, f32)>, PluginError> {
    let params: [(&str, &str); 5] = [
        ("method", "album.search"),
        ("album", album),
        ("api_key", api_key),
        ("format", "json"),
        ("limit", "5"),
    ];
    let body = lastfm_get(&params)?;
    Ok(pick_best_album_search_match(&body, album, artist))
}

/// 从 query.mbid 或已知 musicbrainz ID 解析 MBID（对应原生 resolve_mbid）。
fn resolve_mbid(q: &EntityQuery) -> Option<String> {
    q.mbid
        .as_deref()
        .and_then(normalize_mbid)
        .or_else(|| q.known_id("musicbrainz").and_then(normalize_mbid))
}

fn fetch_artist(
    q: &EntityQuery,
    api_key: &str,
    language: &str,
) -> Result<ScrapeResult, PluginError> {
    let mbid = resolve_mbid(q);

    let (resolved_mbid, search_confidence) = if let Some(m) = mbid {
        (Some(m), 1.0f32)
    } else if let Some(name) = q.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        match search_best_mbid(api_key, name)? {
            Some((m, conf)) if conf >= CONFIDENCE_THRESHOLD => (Some(m), conf),
            _ => return Ok(ScrapeResult::default()),
        }
    } else {
        return Ok(ScrapeResult::default());
    };

    let mbid_ref = resolved_mbid.as_deref();
    let name_ref = q.name.as_deref();

    let body = get_info(api_key, mbid_ref, name_ref, language)?;
    let mut bio = parse_artist_bio(&body);
    if bio.as_ref().is_none_or(|b| b.text.trim().is_empty()) && language != "en" {
        let en_body = get_info(api_key, mbid_ref, name_ref, "en")?;
        bio = parse_artist_bio(&en_body);
    }

    let image = parse_artist_image(&body);
    let external_ids = parse_artist_ids(&body, mbid_ref);
    let confidence = if bio.is_some() || image.is_some() || !external_ids.is_empty() {
        search_confidence
    } else {
        0.0
    };

    Ok(ScrapeResult {
        external_ids,
        bio,
        image,
        confidence,
        ..Default::default()
    })
}

fn fetch_album(
    q: &EntityQuery,
    api_key: &str,
    language: &str,
) -> Result<ScrapeResult, PluginError> {
    let Some(album_name) = q.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(ScrapeResult::default());
    };
    let album_name = album_name.to_string();
    let artist_name = q
        .artist_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let (resolved_artist, resolved_album, search_confidence) = if let Some(artist) = artist_name {
        (artist, album_name, 1.0f32)
    } else {
        match search_best_album(api_key, &album_name, None)? {
            Some((artist, album, conf)) if conf >= CONFIDENCE_THRESHOLD => (artist, album, conf),
            _ => return Ok(ScrapeResult::default()),
        }
    };

    let body = album_get_info(api_key, &resolved_artist, &resolved_album, language)?;
    let mut bio = parse_album_description(&body);
    if bio.as_ref().is_none_or(|b| b.text.trim().is_empty()) && language != "en" {
        let en_body = album_get_info(api_key, &resolved_artist, &resolved_album, "en")?;
        bio = parse_album_description(&en_body);
    }

    let image = parse_album_cover(&body);
    let external_ids = parse_album_ids(&body);
    let confidence = if bio.is_some() || image.is_some() || !external_ids.is_empty() {
        search_confidence
    } else {
        0.0
    };

    Ok(ScrapeResult {
        external_ids,
        bio,
        image,
        confidence,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// 响应解析（与原生逐字对应）
// ---------------------------------------------------------------------------

/// Last.fm 200 响应体内的 `error` / `message`（语义等同 4xx，不重试）。
fn parse_api_error(body: &Value) -> Option<String> {
    let code = body.get("error")?;
    let msg = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Last.fm API error");
    Some(format!("Last.fm error {}: {}", code, msg))
}

/// 从 `artist.getInfo` 响应解析简介（去 HTML + 去 Last.fm 尾巴）。
fn parse_artist_bio(body: &Value) -> Option<Bio> {
    let artist = body.get("artist")?;
    let bio_obj = artist.get("bio")?;
    let raw = bio_obj
        .get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| bio_obj.get("content").and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())?;
    let text = trim_lastfm_bio_tail(&strip_html(raw));
    if text.trim().is_empty() {
        return None;
    }
    Some(Bio {
        text,
        source: "lastfm".into(),
    })
}

/// 解析 external_id：优先 mbid，否则 url slug。
fn parse_artist_ids(body: &Value, fallback_mbid: Option<&str>) -> Vec<FetchedId> {
    let Some(artist) = body.get("artist") else {
        return Vec::new();
    };
    let url = artist
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mbid = artist
        .get("mbid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| fallback_mbid.map(str::to_string));
    let external_id = mbid
        .clone()
        .or_else(|| url.as_deref().and_then(extract_lastfm_slug))
        .unwrap_or_else(|| {
            artist
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string()
        });
    vec![FetchedId {
        provider: "lastfm".into(),
        external_id,
        url,
    }]
}

/// `artist.search` 取名称相似度最高且 ≥ 阈值的 MBID。
fn pick_best_search_match(body: &Value, query_name: &str) -> Option<(String, f32)> {
    let artists = body
        .get("results")
        .and_then(|r| r.get("artistmatches"))
        .and_then(|m| m.get("artist"))
        .and_then(|a| a.as_array())
        .cloned()
        .or_else(|| {
            body.get("results")
                .and_then(|r| r.get("artistmatches"))
                .and_then(|m| m.get("artist"))
                .map(|a| vec![a.clone()])
        })?;
    let mut best: Option<(String, f32)> = None;
    for item in artists {
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let sim = name_similarity(query_name, name);
        let mbid = item
            .get("mbid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(mbid) = mbid else { continue };
        if sim >= CONFIDENCE_THRESHOLD && best.as_ref().is_none_or(|(_, b)| sim > *b) {
            best = Some((mbid, sim));
        }
    }
    best
}

/// 从 `album.getInfo` 响应解析简介。
fn parse_album_description(body: &Value) -> Option<Bio> {
    let album = body.get("album")?;
    let wiki = album.get("wiki")?;
    let raw = wiki
        .get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| wiki.get("content").and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())?;
    let text = trim_lastfm_bio_tail(&strip_html(raw));
    if text.trim().is_empty() {
        return None;
    }
    Some(Bio {
        text,
        source: "lastfm".into(),
    })
}

fn lastfm_remote_image(url: String) -> RemoteImage {
    RemoteImage {
        url,
        license: None,
        attribution: None,
        source: "lastfm".into(),
    }
}

/// 从 Last.fm `image` 数组取最大可用 URL；空串与默认占位图跳过。
fn pick_lastfm_image_url(images: &Value) -> Option<String> {
    let images = images.as_array()?;
    const ORDER: [&str; 5] = ["mega", "extralarge", "large", "medium", "small"];
    for size in ORDER {
        if let Some(url) = images.iter().find_map(|img| {
            let matches = img.get("size").and_then(|v| v.as_str()) == Some(size);
            if !matches {
                return None;
            }
            let url = img
                .get("#text")
                .or_else(|| img.get("url"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.contains(LASTFM_PLACEHOLDER_HASH))?;
            Some(url.to_string())
        }) {
            return Some(url);
        }
    }
    None
}

/// 从 `artist.getInfo` 的 image 数组取最大尺寸头像 URL。
fn parse_artist_image(body: &Value) -> Option<RemoteImage> {
    pick_lastfm_image_url(body.get("artist")?.get("image")?).map(lastfm_remote_image)
}

/// 从 `album.getInfo` 的 image 数组取最大尺寸封面 URL。
fn parse_album_cover(body: &Value) -> Option<RemoteImage> {
    pick_lastfm_image_url(body.get("album")?.get("image")?).map(lastfm_remote_image)
}

/// 解析专辑 external_id（url slug）。
fn parse_album_ids(body: &Value) -> Vec<FetchedId> {
    let Some(album) = body.get("album") else {
        return Vec::new();
    };
    let url = album
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let external_id = url
        .as_deref()
        .and_then(extract_lastfm_slug)
        .unwrap_or_else(|| {
            album
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string()
        });
    vec![FetchedId {
        provider: "lastfm".into(),
        external_id,
        url,
    }]
}

/// `album.search` 取专辑名相似度最高且 ≥ 阈值的 artist+album。
fn pick_best_album_search_match(
    body: &Value,
    query_album: &str,
    query_artist: Option<&str>,
) -> Option<(String, String, f32)> {
    let albums = body
        .get("results")
        .and_then(|r| r.get("albummatches"))
        .and_then(|m| m.get("album"))
        .and_then(|a| a.as_array())
        .cloned()
        .or_else(|| {
            body.get("results")
                .and_then(|r| r.get("albummatches"))
                .and_then(|m| m.get("album"))
                .map(|a| vec![a.clone()])
        })?;
    let mut best: Option<(String, String, f32)> = None;
    for item in albums {
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let artist = item.get("artist").and_then(|v| v.as_str()).unwrap_or("");
        let album_sim = name_similarity(query_album, name);
        let artist_sim = query_artist
            .map(|qa| name_similarity(qa, artist))
            .unwrap_or(1.0);
        let sim = (album_sim + artist_sim) / 2.0;
        if sim >= CONFIDENCE_THRESHOLD && best.as_ref().is_none_or(|(_, _, b)| sim > *b) {
            best = Some((artist.to_string(), name.to_string(), sim));
        }
    }
    best
}

/// 从 `https://www.last.fm/music/The+Beatles` 提取 slug。
fn extract_lastfm_slug(url: &str) -> Option<String> {
    let path = url.trim_end_matches('/');
    let slug = path.rsplit('/').next()?;
    if slug.is_empty() || slug == "music" {
        return None;
    }
    Some(slug.to_string())
}

/// 去掉简单 HTML 标签并解码常见实体。
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_html_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// 去掉 “Read more on Last.fm” 及 User-contributed 等尾巴。
fn trim_lastfm_bio_tail(text: &str) -> String {
    let markers = [
        "Read more on Last.fm",
        "Read more on Last.fm.",
        "User-contributed text is available under the Creative Commons By-SA License",
    ];
    let mut cut = text;
    for m in markers {
        if let Some(idx) = cut.find(m) {
            cut = &cut[..idx];
        }
    }
    cut.trim().trim_end_matches('.').trim().to_string()
}
