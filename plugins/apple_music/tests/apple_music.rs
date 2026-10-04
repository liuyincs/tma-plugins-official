//! apple_music 官方插件验收测试（Bearer token / storefront 路径 / 已知 ID 短路）。
//!
//! 断言逐条移植自 TMA 主仓 `crates/scrape/src/builtin_tests/apple_music.rs`；
//! 夹具由 `testkit` 提供。

use std::sync::Arc;

use tma_plugin_sdk::PluginErrorCode;
use testkit::{RouteBuilder, StubProxy, artist_query, artist_query_with_known};

/// 构建 wasm → 测试私钥 pack → 验签 → 实例化（`tma_config` 返回 runtime_config 原样）。
fn load(runtime_config: &str, proxy: Arc<StubProxy>) -> testkit::LoadedPlugin {
    testkit::load_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        runtime_config,
        proxy,
    )
}

/// 目录搜索：Bearer developer token + storefront 进路径；置信度 0.9。
#[test]
fn apple_music_search_bearer_token_and_storefront() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "api.music.apple.com/v1/catalog/jp/search",
        serde_json::json!({
            "results": { "artists": { "data": [{
                "id": "apple-artist-123",
                "attributes": { "url": "https://music.apple.com/jp/artist/x/123" }
            }]}}
        }),
    )]);
    let provider = load(
        r#"{"developer_token":"dev-tok","storefront":"jp"}"#,
        proxy.clone(),
    );

    let r = provider
        .call_scrape(artist_query(None, Some("Some Artist")))
        .unwrap();

    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].provider, "apple_music");
    assert_eq!(r.external_ids[0].external_id, "apple-artist-123");
    assert_eq!(
        r.external_ids[0].url.as_deref(),
        Some("https://music.apple.com/jp/artist/x/123")
    );
    assert!((r.confidence - 0.9).abs() < f32::EPSILON, "搜索命中 0.9");

    let reqs = proxy.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer dev-tok"),
        "developer token 走 Bearer 头"
    );
    assert!(
        reqs[0]
            .url
            .contains("/v1/catalog/jp/search?term=Some%20Artist&types=artists&limit=1"),
        "storefront 进路径、term 编码: {}",
        reqs[0].url
    );
}

/// 已知 Apple ID：直接复用，零外呼。
#[test]
fn apple_music_known_id_short_circuits_zero_outbound() {
    let proxy = StubProxy::new(vec![]);
    let provider = load(
        r#"{"developer_token":"dev-tok"}"#,
        proxy.clone(),
    );

    let r = provider
        .call_scrape(artist_query_with_known(
            None,
            Some("Some Artist"),
            &[("apple_music", "apple-artist-123")],
        ))
        .unwrap();

    assert_eq!(r.external_ids.len(), 1);
    assert_eq!(r.external_ids[0].external_id, "apple-artist-123");
    assert!((r.confidence - 1.0).abs() < f32::EPSILON);
    assert!(proxy.requests().is_empty(), "已知 ID 不应外呼");
}

/// 未配置 developer_token：PermanentFailure 且零外呼。
#[test]
fn apple_music_missing_token_fails_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let provider = load("{}", proxy.clone());

    let err = provider
        .call_scrape(artist_query(None, Some("Some Artist")))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::PermanentFailure);
    assert!(proxy.requests().is_empty(), "缺凭据不应外呼");
}
