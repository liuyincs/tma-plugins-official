//! listenbrainz 官方插件验收测试（authorize 动作 + scrobble/now_playing 事件）。
//!
//! 与其他插件同走 `testkit`：真实签名 .tmap 装载（构建 wasm → 测试私钥
//! pack → 验签 → extism 实例化），出站经 StubProxy 罐头响应并留痕断言。
//! 本文件是全仓首个 `call_event`/`call_action` 驱动面用例（其余插件测试
//! 只覆盖 `call_scrape`/`call_http`）。

use std::collections::BTreeMap;
use std::sync::Arc;

use testkit::{
    LibraryType, NowPlayingEventPayload, PluginActionRequest, PluginErrorCode, PluginEventRequest,
    Route, RouteBuilder, ScrobbleEventPayload, StubProxy, TrackEventFields,
};

/// 构建 wasm → 测试私钥 pack → 验签 → 实例化（`tma_config` 返回 runtime_config 原样）。
fn load(runtime_config: &str, proxy: Arc<StubProxy>) -> testkit::Driver {
    testkit::HostSim::new()
        .config(runtime_config)
        .proxy(proxy)
        .load_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// 与 src/scrobble.rs 单测同基线的曲目载荷（213s、无可选元数据）。
fn track() -> TrackEventFields {
    TrackEventFields {
        library_type: LibraryType::Music,
        track_title: "曲名".into(),
        primary_artist: "主艺人".into(),
        artists: vec![],
        album_name: None,
        album_artist: None,
        track_number: None,
        disc_number: None,
        year: None,
        duration_seconds: Some(213),
        recording_mbid: None,
        artist_mbids: None,
        album_mbid: None,
    }
}

fn scrobble(track: TrackEventFields) -> PluginEventRequest {
    PluginEventRequest::Scrobble(ScrobbleEventPayload {
        event_id: "evt-1".into(),
        track,
        played_at: 1_735_689_600,
    })
}

fn now_playing(track: TrackEventFields) -> PluginEventRequest {
    PluginEventRequest::NowPlaying(NowPlayingEventPayload {
        track,
        position_ms: 42_000,
        duration_ms: 213_500,
    })
}

fn authorize(locale: Option<&str>) -> PluginActionRequest {
    PluginActionRequest {
        action_id: "authorize".into(),
        payload: None,
        locale: locale.map(str::to_string),
    }
}

/// scrobble 事件 → `POST /1/submit-listens`：`Token` 认证头 + JSON body
/// `listen_type=single`（`listened_at` 透传事件 played_at）。
#[test]
fn scrobble_posts_single_listen_with_token_auth() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "api.listenbrainz.org/1/submit-listens",
        serde_json::json!({}),
    )]);
    let plugin = load(r#"{"token":"lb-token"}"#, proxy.clone());

    let resp = plugin.call_event(scrobble(track())).unwrap();
    assert!(resp.ok, "scrobble 应被接受: {resp:?}");

    let requests = plugin.http_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.url, "https://api.listenbrainz.org/1/submit-listens");
    assert_eq!(req.headers["Authorization"], "Token lb-token");
    assert_eq!(req.headers["Content-Type"], "application/json");
    let body: serde_json::Value =
        serde_json::from_slice(req.body.as_deref().expect("scrobble 必带 JSON body")).unwrap();
    assert_eq!(body["listen_type"], "single");
    assert_eq!(body["payload"][0]["listened_at"], 1_735_689_600);
    assert_eq!(body["payload"][0]["track_metadata"]["track_name"], "曲名");
}

/// now_playing 事件 → `listen_type=playing_now`（无 listened_at），
/// additional_info 带播放器口径进度毫秒。
#[test]
fn now_playing_posts_playing_now_without_listened_at() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "api.listenbrainz.org/1/submit-listens",
        serde_json::json!({}),
    )]);
    let plugin = load(r#"{"token":"lb-token"}"#, proxy.clone());

    let resp = plugin.call_event(now_playing(track())).unwrap();
    assert!(resp.ok);

    let requests = plugin.http_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.url, "https://api.listenbrainz.org/1/submit-listens");
    assert_eq!(req.headers["Authorization"], "Token lb-token");
    let body: serde_json::Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["listen_type"], "playing_now");
    assert!(body["payload"][0].get("listened_at").is_none());
    // 进度两个字段都取播放器口径：处理器误传标签时长（213_000）会在此显红。
    let info = &body["payload"][0]["track_metadata"]["additional_info"];
    assert_eq!(info["position_ms"], 42_000);
    assert_eq!(info["duration_ms"], 213_500);
}

/// 未配置 token：事件拒绝（`InvalidArgument` 不可重试），零外呼。
#[test]
fn missing_token_rejects_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let plugin = load("{}", proxy.clone());

    let resp = plugin.call_event(scrobble(track())).unwrap();
    assert!(!resp.ok);
    let err = resp.error.expect("拒绝应带 error");
    assert_eq!(err.code, PluginErrorCode::InvalidArgument);
    assert!(!err.retryable);
    assert!(plugin.http_requests().is_empty(), "拒绝前不得外呼");
}

/// <30s 曲目直接接受跳过（不计失败），零外呼。
#[test]
fn short_track_accepted_without_outbound() {
    let proxy = StubProxy::new(vec![]);
    let plugin = load(r#"{"token":"lb-token"}"#, proxy.clone());

    let mut short = track();
    short.duration_seconds = Some(29);
    let resp = plugin.call_event(scrobble(short)).unwrap();
    assert!(resp.ok);
    assert!(plugin.http_requests().is_empty());
}

/// 上游 401 → 拒绝 `InvalidArgument`（不可重试），错误文案指 token 无效。
#[test]
fn submit_401_rejects_invalid_token() {
    let proxy = StubProxy::new(vec![Route {
        needle: "api.listenbrainz.org/1/submit-listens".into(),
        status: 401,
        headers: BTreeMap::new(),
        body: r#"{"code":401,"error":"Invalid authorization token."}"#.into(),
    }]);
    let plugin = load(r#"{"token":"bad-token"}"#, proxy.clone());

    let resp = plugin.call_event(scrobble(track())).unwrap();
    assert!(!resp.ok);
    let err = resp.error.unwrap();
    assert_eq!(err.code, PluginErrorCode::InvalidArgument);
    assert!(!err.retryable);
    let requests = plugin.http_requests();
    assert_eq!(requests.len(), 1, "拒绝来自上游 401");
    // StubProxy 按 URL 子串匹配，精确断言请求面防错误域名误命中。
    let req = &requests[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.url, "https://api.listenbrainz.org/1/submit-listens");
    assert_eq!(req.headers["Authorization"], "Token bad-token");
}

/// authorize 动作：`GET /1/validate-token` 成功回 `ok:true` 附账户名；
/// 请求带 `Token` 认证头与 `Accept: application/json`。
#[test]
fn authorize_validates_token_and_reports_user() {
    let proxy = StubProxy::new(vec![RouteBuilder::json(
        "api.listenbrainz.org/1/validate-token",
        serde_json::json!({"valid": true, "user_name": "lb-listener"}),
    )]);
    let plugin = load(r#"{"token":"lb-token"}"#, proxy.clone());

    let resp = plugin.call_action(authorize(Some("en"))).unwrap();
    assert!(resp.ok, "authorize 应成功: {}", resp.message);
    assert!(resp.message.contains("lb-listener"));

    let requests = plugin.http_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.method, "GET");
    assert_eq!(req.url, "https://api.listenbrainz.org/1/validate-token");
    assert_eq!(req.headers["Authorization"], "Token lb-token");
    assert_eq!(req.headers["Accept"], "application/json");
}

/// authorize 遇 401 → `ok:false` 且文案区分 token 无效（非网络错）。
#[test]
fn authorize_401_reports_invalid_token() {
    let proxy = StubProxy::new(vec![Route {
        needle: "api.listenbrainz.org/1/validate-token".into(),
        status: 401,
        headers: BTreeMap::new(),
        body: r#"{"code":401,"error":"Invalid authorization token."}"#.into(),
    }]);
    let plugin = load(r#"{"token":"bad-token"}"#, proxy.clone());

    let resp = plugin.call_action(authorize(Some("zh"))).unwrap();
    assert!(!resp.ok);
    assert!(
        resp.message.contains("token 无效"),
        "中文文案应指 token 无效: {}",
        resp.message
    );

    let requests = plugin.http_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.method, "GET");
    assert_eq!(req.url, "https://api.listenbrainz.org/1/validate-token");
    assert_eq!(req.headers["Authorization"], "Token bad-token");
}

/// 未配置 token 时 authorize 不外呼，直接 `ok:false` 提示先配置。
#[test]
fn authorize_without_token_fails_before_outbound() {
    let proxy = StubProxy::new(vec![]);
    let plugin = load("{}", proxy.clone());

    let resp = plugin.call_action(authorize(None)).unwrap();
    assert!(!resp.ok);
    assert!(plugin.http_requests().is_empty());
}
