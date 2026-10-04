//! spotify 官方插件验收测试（token POST / 名称与 ISRC 搜索 / 401 地理封锁）。
//!
//! 断言逐条移植自 TMA 主仓 `crates/scrape/src/builtin_tests/spotify.rs`；
//! 夹具由 `testkit` 提供（stub 代理 / `tma_config` 配置注入 / 真实签名包加载）。

use std::collections::BTreeMap;
use std::sync::Arc;

use tma_plugin_sdk::{EntityQuery, PluginErrorCode, ScrapeEntityKind, base64_encode};
use testkit::{Route, RouteBuilder, StubProxy, artist_query};

/// 构建 wasm → 测试私钥 pack → 验签 → 实例化（`tma_config` 返回 runtime_config 原样）。
fn load(runtime_config: &str, proxy: Arc<StubProxy>) -> testkit::LoadedPlugin {
    testkit::load_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        runtime_config,
        proxy,
    )
}

/// token POST（Basic + form body）→ 名称搜索（Bearer）；0.8 置信度。
#[test]
fn spotify_token_then_artist_search_with_auth_headers() {
    let proxy = StubProxy::new(vec![
        RouteBuilder::json(
            "accounts.spotify.com/api/token",
            serde_json::json!({ "access_token": "tok-123", "expires_in": 3600 }),
        ),
        RouteBuilder::json(
            "api.spotify.com/v1/search",
            serde_json::json!({ "artists": { "items": [{ "id": "spotify-artist-1" }] } }),
        ),
    ]);
    let provider = load(
        r#"{"client_id":"my-id","client_secret":"my-secret"}"#,
        proxy.clone(),
    );

    let r = provider
        .call_scrape(artist_query(None, Some("Some Artist")))
        .unwrap();

    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].provider, "spotify");
    assert_eq!(r.external_ids[0].external_id, "spotify-artist-1");
    assert_eq!(
        r.external_ids[0].url.as_deref(),
        Some("https://open.spotify.com/artist/spotify-artist-1")
    );
    assert!((r.confidence - 0.8).abs() < f32::EPSILON, "名称搜索 0.8");

    let reqs = proxy.requests();
    assert_eq!(reqs.len(), 2, "token → search");

    // token：POST + Basic base64(id:secret) + form body。
    let token_req = &reqs[0];
    assert_eq!(token_req.method, "POST");
    assert_eq!(token_req.url, "https://accounts.spotify.com/api/token");
    let expected_basic = base64_encode(b"my-id:my-secret");
    assert_eq!(
        token_req.headers.get("Authorization").map(String::as_str),
        Some(format!("Basic {expected_basic}").as_str()),
        "client credentials 走 Basic 头"
    );
    assert_eq!(
        token_req.headers.get("Content-Type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        token_req.body.as_deref(),
        Some(&b"grant_type=client_credentials"[..]),
        "token 请求体为 client_credentials form"
    );

    // search：Bearer token + term 编码。
    let search_req = &reqs[1];
    assert_eq!(
        search_req.headers.get("Authorization").map(String::as_str),
        Some("Bearer tok-123")
    );
    assert!(
        search_req
            .url
            .contains("/v1/search?q=Some%20Artist&type=artist&limit=1"),
        "名称查 artist: {}",
        search_req.url
    );
}

/// ISRC 查 track（0.9 置信度）：track → 其首个 artist 的 ID。
#[test]
fn spotify_isrc_track_query_resolves_first_artist() {
    let proxy = StubProxy::new(vec![
        RouteBuilder::json(
            "accounts.spotify.com/api/token",
            serde_json::json!({ "access_token": "tok-123", "expires_in": 3600 }),
        ),
        RouteBuilder::json(
            "api.spotify.com/v1/search",
            serde_json::json!({
                "tracks": { "items": [{ "artists": [{ "id": "spotify-via-track" }] }] }
            }),
        ),
    ]);
    let provider = load(
        r#"{"client_id":"my-id","client_secret":"my-secret"}"#,
        proxy.clone(),
    );

    let query = EntityQuery {
        kind: ScrapeEntityKind::Artist,
        mbid: None,
        isrc: Some("JPU901700876".into()),
        name: Some("Whatever".into()),
        artist_name: None,
        known_external_ids: Vec::new(),
        library_types: Vec::new(),
    };
    let r = provider.call_scrape(query).unwrap();

    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].external_id, "spotify-via-track");
    assert!(
        (r.confidence - 0.9).abs() < f32::EPSILON,
        "ISRC 高于名称匹配，实际 {}",
        r.confidence
    );
    let urls = proxy.urls();
    assert!(
        urls[1].contains("q=isrc%3AJPU901700876&type=track&limit=1"),
        "ISRC 查 track: {}",
        urls[1]
    );
}

/// 搜索 401：地理封锁语义 → PermanentFailure（插件侧不冷却，交管线退避）。
#[test]
fn spotify_401_maps_to_geo_restriction_permanent_failure() {
    let proxy = StubProxy::new(vec![
        RouteBuilder::json(
            "accounts.spotify.com/api/token",
            serde_json::json!({ "access_token": "tok-123", "expires_in": 3600 }),
        ),
        Route {
            needle: "api.spotify.com/v1/search".into(),
            status: 401,
            headers: BTreeMap::new(),
            body: "{\"error\":{\"message\":\"Token permissions\"}}".into(),
        },
    ]);
    let provider = load(
        r#"{"client_id":"my-id","client_secret":"my-secret"}"#,
        proxy,
    );

    let err = provider
        .call_scrape(artist_query(None, Some("Some Artist")))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(
        err.message.contains("401") && err.message.contains("地理封锁"),
        "应带地理封锁语义: {}",
        err.message
    );
}

/// 未配置 client credentials：PermanentFailure 且零外呼。
#[test]
fn spotify_missing_credentials_fails_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let provider = load("{}", proxy.clone());

    let err = provider
        .call_scrape(artist_query(None, Some("Some Artist")))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(proxy.requests().is_empty(), "缺凭据不应外呼");
}
