//! fanart 官方插件验收测试（thumb 主图 / likes 最高多套图 / 缺凭据零外呼）。
//!
//! 断言逐条移植自 TMA 主仓 `crates/scrape/src/builtin_tests/fanart.rs`；
//! 夹具由 `testkit` 提供。

use std::sync::Arc;

use tma_plugin_sdk::PluginErrorCode;
use testkit::{BEATLES_MBID, RouteBuilder, StubProxy, artist_query};

/// 构建 wasm → 测试私钥 pack → 验签 → 实例化（`tma_config` 返回 runtime_config 原样）。
fn load(runtime_config: &str, proxy: Arc<StubProxy>) -> testkit::LoadedPlugin {
    testkit::load_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        runtime_config,
        proxy,
    )
}

/// thumb 作主图 + 各 kind 取 likes 最高；hdmusiclogo 优先于 musiclogo。
#[test]
fn fanart_thumb_main_and_max_likes_extras() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "webservice.fanart.tv/v3/music/",
        serde_json::json!({
            "artistthumb": [
                { "url": "https://fanart.tv/t1.jpg", "likes": "2" },
                { "url": "https://fanart.tv/t7.jpg", "likes": "7" }
            ],
            "artistbackground": [{ "url": "https://fanart.tv/bg.jpg", "likes": "1" }],
            "hdmusiclogo": [{ "url": "https://fanart.tv/hdlogo.png", "likes": "3" }],
            "musiclogo": [{ "url": "https://fanart.tv/logo.png", "likes": "9" }],
            "musicbanner": [{ "url": "https://fanart.tv/banner.jpg", "likes": "5" }]
        }),
    )]);
    let provider = load(r#"{"api_key":"fanart-key"}"#, proxy.clone());

    let r = provider
        .call_scrape(artist_query(Some(BEATLES_MBID), None))
        .unwrap();

    // 主图 = likes 最高的 thumb；license 固定 Fanart.tv。
    let img = r.image.expect("thumb 应作主图");
    assert_eq!(img.url, "https://fanart.tv/t7.jpg");
    assert_eq!(img.license.as_deref(), Some("Fanart.tv"));
    assert_eq!(img.source, "fanart");
    assert!((r.confidence - 1.0).abs() < f32::EPSILON);

    // extra_images：thumb / artistbackground / musiclogo（hdmusiclogo 优先，
    // 即使 musiclogo likes 更高）/ musicbanner，各取 likes 最高。
    let extras: Vec<(&str, &str)> = r
        .extra_images
        .iter()
        .map(|e| (e.kind.as_str(), e.url.as_str()))
        .collect();
    assert_eq!(
        extras,
        vec![
            ("thumb", "https://fanart.tv/t7.jpg"),
            ("artistbackground", "https://fanart.tv/bg.jpg"),
            ("musiclogo", "https://fanart.tv/hdlogo.png"),
            ("musicbanner", "https://fanart.tv/banner.jpg"),
        ]
    );
    assert!(r.extra_images.iter().all(|e| e.source == "fanart"));

    // external_id = MBID；api_key 拼在 query。
    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].external_id, BEATLES_MBID);
    let urls = proxy.urls();
    assert_eq!(urls.len(), 1);
    assert!(
        urls[0].contains(&format!(
            "https://webservice.fanart.tv/v3/music/{BEATLES_MBID}?api_key=fanart-key"
        )),
        "URL 应为 /v3/music/<mbid>?api_key=…: {}",
        urls[0]
    );
}

/// 未配置 api_key：PermanentFailure 且零外呼。
#[test]
fn fanart_missing_api_key_fails_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let provider = load("{}", proxy.clone());

    let err = provider
        .call_scrape(artist_query(Some(BEATLES_MBID), None))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(proxy.requests().is_empty(), "缺凭据不应外呼");
}
