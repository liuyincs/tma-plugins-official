//! lastfm 官方插件验收测试（bio 语言回退 / body error / 缺凭据零外呼）。
//!
//! 断言逐条移植自 TMA 主仓 `crates/scrape/src/builtin_tests/lastfm.rs`；
//! 夹具由 `testkit` 提供。

use std::collections::BTreeMap;
use std::sync::Arc;

use testkit::{BEATLES_MBID, PluginErrorCode, Route, RouteBuilder, StubProxy, artist_query};

/// 构建 wasm → 测试私钥 pack → 验签 → 实例化（`tma_config` 返回 runtime_config 原样）。
fn load(runtime_config: &str, proxy: Arc<StubProxy>) -> testkit::LoadedPlugin {
    testkit::load_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        runtime_config,
        proxy,
    )
}

/// bio 中文缺失 → 英文重取；图片取最大尺寸；MBID 直落；api_key 进 query。
#[test]
fn lastfm_bio_falls_back_to_en_and_emits_image_ids_and_url() {
    let zh_body = serde_json::json!({
        "artist": {
            "name": "The Beatles",
            "mbid": BEATLES_MBID,
            "url": "https://www.last.fm/music/The+Beatles",
            "image": [
                { "size": "small", "#text": "https://img.lastfm/small.jpg" },
                { "size": "mega", "#text": "https://img.lastfm/mega.jpg" }
            ],
            "bio": { "summary": "", "content": "" }
        }
    });
    let en_body = serde_json::json!({
        "artist": {
            "bio": { "summary": "English bio <a href=\"x\">link</a> Read more on Last.fm." }
        }
    });
    let proxy = StubProxy::new(vec![
        Route {
            needle: "lang=zh".into(),
            status: 200,
            headers: BTreeMap::new(),
            body: zh_body.to_string(),
        },
        Route {
            needle: "lang=en".into(),
            status: 200,
            headers: BTreeMap::new(),
            body: en_body.to_string(),
        },
    ]);
    let provider = load(r#"{"api_key":"test-key","language":"zh"}"#, proxy.clone());

    let r = provider
        .call_scrape(artist_query(Some(BEATLES_MBID), Some("The Beatles")))
        .unwrap();

    // bio：zh 空回退 en；HTML 剥离 + Last.fm 尾巴截断。
    let bio = r.bio.expect("en 回退应产出 bio");
    assert_eq!(bio.text, "English bio link");
    assert_eq!(bio.source, "lastfm");
    // 图片：mega 优先于 small；无 license（Last.fm 无此语义）。
    let img = r.image.expect("应解析图片");
    assert_eq!(img.url, "https://img.lastfm/mega.jpg");
    assert_eq!(img.source, "lastfm");
    assert!(img.license.is_none());
    // external_id：优先 body mbid；URL 原样带出。
    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].provider, "lastfm");
    assert_eq!(r.external_ids[0].external_id, BEATLES_MBID);
    assert_eq!(
        r.external_ids[0].url.as_deref(),
        Some("https://www.last.fm/music/The+Beatles")
    );
    assert!((r.confidence - 1.0).abs() < f32::EPSILON, "MBID 直查 1.0");

    // 两次 getInfo：lang=zh 与 lang=en；api_key 原样拼 query。
    let urls = proxy.urls();
    assert_eq!(urls.len(), 2);
    assert!(urls[0].starts_with("https://ws.audioscrobbler.com/2.0/"));
    assert!(
        urls.iter()
            .all(|u| u.contains("method=artist.getInfo") && u.contains("api_key=test-key")),
        "api_key 应随每个请求: {urls:?}"
    );
    assert!(urls[0].contains("lang=zh") && urls[1].contains("lang=en"));
}

/// Last.fm 200 响应体内的 error → PermanentFailure（不重试）。
#[test]
fn lastfm_body_error_maps_to_permanent_failure() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "ws.audioscrobbler.com",
        serde_json::json!({ "error": 6, "message": "The artist you supplied could not be found" }),
    )]);
    let provider = load(r#"{"api_key":"k"}"#, proxy);

    let err = provider
        .call_scrape(artist_query(Some(BEATLES_MBID), None))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(!err.retryable);
    assert!(
        err.message.contains("Last.fm error 6"),
        "应透传 body error/message: {}",
        err.message
    );
}

/// 未配置 api_key：PermanentFailure 且零外呼（原生 new() 返回 None 的等价语义）。
#[test]
fn lastfm_missing_api_key_fails_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let provider = load("{}", proxy.clone());

    let err = provider
        .call_scrape(artist_query(Some(BEATLES_MBID), None))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(err.message.contains("api_key"));
    assert!(proxy.requests().is_empty(), "缺凭据不应外呼");
}
