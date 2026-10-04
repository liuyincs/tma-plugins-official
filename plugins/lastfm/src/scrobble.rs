//! lastfm 插件的上报纯逻辑（0.3.0 起）：Last.fm web API 签名、
//! track.scrobble / track.updateNowPlaying / auth.getToken / auth.getSession
//! 的请求组装、body 级 error 码表映射与 <30s 跳过判据。
//!
//! 本模块**双 target 编译**（宿主 `cargo test -p tma-builtin-lastfm` 跑单测；
//! wasm 侧经 `crate::plugin` 引用），因此只允许纯数据变换：不声明 extism 宿主
//! 函数、不触碰 transport / read_config（那些只在 wasm target 存在）。
//!
//! 签名算法（官方 auth 规则）：除 `format` 外全部请求参数按键名排序，按
//! `k`+`v` 无分隔拼接，末尾拼 shared_secret，取 MD5 十六进制即 `api_sig`。

use std::collections::BTreeMap;

use md5::{Digest, Md5};
use serde_json::Value;
use tma_plugin_sdk::{
    PluginError, PluginErrorCode, TrackEventFields, percent_encode_query, should_skip_scrobble,
};

/// 过短曲目不上报（与 `tma_plugin_sdk::MIN_SCROBBLE_SECONDS` 同源）。
/// 多艺术家口径（`tma_plugin_sdk::join_artists` 再导出，供单测）。
pub(crate) use tma_plugin_sdk::join_artists;

// ---------------------------------------------------------------------------
// 签名
// ---------------------------------------------------------------------------

/// MD5 十六进制（小写）：api_sig 与通用摘要共用。
pub(crate) fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 官方 api_sig：参数（除 format 外）按键名排序拼 `k`+`v`，末尾拼 shared_secret。
///
/// `BTreeMap<&str, String>` 的键序即签名要求的字节序排序；哈希输入用**未编码**
/// 原始值（百分号编码只发生在组装 body 时）。
pub(crate) fn api_sig(params: &BTreeMap<&str, String>, shared_secret: &str) -> String {
    let mut concat = String::new();
    for (k, v) in params {
        concat.push_str(k);
        concat.push_str(v);
    }
    concat.push_str(shared_secret);
    md5_hex(&concat)
}

/// 组装已签名的 urlencoded form body：`api_sig` 参与签名后追加，`format=json`
/// 只入 body 不参与签名（官方规则），二者都追加在排序参数之后。
pub(crate) fn build_signed_form_body(
    params: &BTreeMap<&str, String>,
    shared_secret: &str,
) -> String {
    let sig = api_sig(params, shared_secret);
    let mut pairs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode_query(k), percent_encode_query(v)))
        .collect();
    pairs.push(format!("api_sig={}", percent_encode_query(&sig)));
    pairs.push("format=json".to_string());
    pairs.join("&")
}

// ---------------------------------------------------------------------------
// 事件载荷 → 请求参数
// ---------------------------------------------------------------------------

/// <30s 跳过判据（`tma_plugin_sdk::should_skip_scrobble` 再导出）。
pub(crate) fn should_skip(duration_seconds: Option<u64>) -> bool {
    should_skip_scrobble(duration_seconds)
}

/// 组装 track.scrobble / track.updateNowPlaying 的已签名 form body。
///
/// `timestamp` 仅 scrobble 携带（updateNowPlaying 无时间戳参数）；Option 字段
/// （album/albumArtist/trackNumber/mbid）缺省或空白时**不传**（Last.fm 对空串
/// 参数按非法值处理，缺参则按未知处理）；`mbid` 取 recording MBID。
pub(crate) fn build_scrobble_body(
    method: &str,
    track: &TrackEventFields,
    timestamp: Option<i64>,
    api_key: &str,
    session_key: &str,
    shared_secret: &str,
) -> String {
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("method", method.to_string());
    params.insert("artist", join_artists(track));
    params.insert("track", track.track_title.trim().to_string());
    if let Some(ts) = timestamp {
        params.insert("timestamp", ts.to_string());
    }
    if let Some(album) = non_empty(track.album_name.as_deref()) {
        params.insert("album", album.to_string());
    }
    if let Some(album_artist) = non_empty(track.album_artist.as_deref()) {
        params.insert("albumArtist", album_artist.to_string());
    }
    if let Some(n) = track.track_number {
        params.insert("trackNumber", n.to_string());
    }
    if let Some(mbid) = non_empty(track.recording_mbid.as_deref()) {
        params.insert("mbid", mbid.to_string());
    }
    params.insert("api_key", api_key.to_string());
    params.insert("sk", session_key.to_string());
    build_signed_form_body(&params, shared_secret)
}

/// 组装 auth.getToken 的已签名 form body。
pub(crate) fn build_get_token_body(api_key: &str, shared_secret: &str) -> String {
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("method", "auth.getToken".to_string());
    params.insert("api_key", api_key.to_string());
    build_signed_form_body(&params, shared_secret)
}

/// 组装 auth.getSession 的已签名 form body（网页授权第二步）。
pub(crate) fn build_get_session_body(token: &str, api_key: &str, shared_secret: &str) -> String {
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("method", "auth.getSession".to_string());
    params.insert("token", token.to_string());
    params.insert("api_key", api_key.to_string());
    build_signed_form_body(&params, shared_secret)
}

/// Last.fm 网页授权页（用户在此登录并允许访问）。
pub(crate) fn authorization_url(api_key: &str, token: &str) -> String {
    format!(
        "https://www.last.fm/api/auth/?api_key={}&token={}",
        percent_encode_query(api_key),
        percent_encode_query(token),
    )
}

/// Last.fm error 14：request token 尚未被用户授权（轮询 complete 的中间态）。
pub(crate) fn is_token_not_authorized(body: &Value) -> bool {
    body.get("error").and_then(Value::as_i64) == Some(14)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// 响应错误映射
// ---------------------------------------------------------------------------

/// Last.fm body 级 error（HTTP 200 + `{"error":N,"message":M}`）→ PluginError。
///
/// 码表（官方 error code 语义 → 宿主重试语义）：
/// - 4（认证失败）/ 6（参数非法）/ 9（会话失效）/ 10、26（key 无效或被封）：
///   凭据/配置类 → `InvalidArgument`（不可重试，重新授权或修配置才有解）；
/// - 29（限流）→ `RateLimited`（可重试）；
/// - 11（服务离线）/ 16（临时错误）→ `Network`（可重试，上游临时故障）；
/// - 其余（2/3/5/7/8/13 等）→ `PermanentFailure`（不可重试）。
///
/// 非 body 级错误（真 HTTP 状态码）走共享 `status_to_error`（429→限流、
/// 5xx→network、其余 4xx→permanent），不在本函数职责内。
pub(crate) fn body_error(body: &Value) -> Option<PluginError> {
    let code = body.get("error").and_then(Value::as_i64)?;
    let msg = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Last.fm API error");
    let text = format!("Last.fm error {code}: {msg}");
    Some(match code {
        4 | 6 | 9 | 10 | 26 => PluginError::new(PluginErrorCode::InvalidArgument, text),
        29 => PluginError::new(PluginErrorCode::RateLimited, text),
        11 | 16 => PluginError::network(text, true),
        _ => PluginError::new(PluginErrorCode::PermanentFailure, text),
    })
}

/// Last.fm error 9（Invalid session key）：网页授权无法静默重换，需用户再点连接。
pub(crate) fn is_invalid_session(err: &PluginError) -> bool {
    err.code == PluginErrorCode::InvalidArgument && err.message.contains("Last.fm error 9:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> TrackEventFields {
        TrackEventFields {
            library_type: tma_plugin_sdk::LibraryType::Music,
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

    // ------------------------- api_sig -------------------------

    /// 官方算法向量：参数按键排序拼 k+v（无分隔），末尾拼 secret，取 MD5。
    /// 期望值由独立实现（Python hashlib）预先算出固定。
    #[test]
    fn api_sig_matches_reference_vectors() {
        let mut params = BTreeMap::new();
        params.insert("method", "auth.getMobileSession".to_string());
        params.insert("username", "user".to_string());
        params.insert("password", "pass".to_string());
        params.insert("api_key", "abc".to_string());
        assert_eq!(
            api_sig(&params, "secret"),
            "0ade3f9005e8c0f38a43d4771a895327"
        );

        let mut params = BTreeMap::new();
        params.insert("method", "track.scrobble".to_string());
        params.insert("artist", "A, B".to_string());
        params.insert("track", "T".to_string());
        params.insert("timestamp", "1700000000".to_string());
        params.insert("api_key", "abc".to_string());
        params.insert("sk", "sess".to_string());
        assert_eq!(api_sig(&params, "shh"), "77f48cc95eabc1ae84077f804cd92916");
    }

    /// 签名按 key 字节序排序（BTreeMap 天然满足），与插入顺序无关。
    #[test]
    fn api_sig_is_key_order_independent() {
        let a: BTreeMap<&str, String> = [("b", "2".to_string()), ("a", "1".to_string())]
            .into_iter()
            .collect();
        let b: BTreeMap<&str, String> = [("a", "1".to_string()), ("b", "2".to_string())]
            .into_iter()
            .collect();
        assert_eq!(api_sig(&a, "s"), api_sig(&b, "s"));
    }

    #[test]
    fn md5_hex_known_digests() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("周杰伦"), "7a8941058aaf4df5147042ce104568da");
    }

    // ------------------------- form body -------------------------

    /// format 不参与签名：body 里手动加 format 后 api_sig 与裸参数向量一致。
    #[test]
    fn signed_body_excludes_format_from_signature() {
        let mut params = BTreeMap::new();
        params.insert("method", "auth.getMobileSession".to_string());
        params.insert("username", "user".to_string());
        params.insert("password", "pass".to_string());
        params.insert("api_key", "abc".to_string());
        let body = build_signed_form_body(&params, "secret");
        assert!(body.contains("api_sig=0ade3f9005e8c0f38a43d4771a895327"));
        assert!(body.contains("format=json"));
        // 值按查询组件百分号编码（空格 %20 而非 +），键序即排序序。
        assert!(body.starts_with("api_key=abc&method=auth.getMobileSession"));
    }

    /// scrobble 参数组装：全字段齐备时的形态（timestamp/album/albumArtist/
    /// trackNumber/mbid=recording_mbid 全带上）。
    #[test]
    fn scrobble_body_carries_all_fields_when_present() {
        let mut t = track();
        t.album_name = Some("专辑".into());
        t.album_artist = Some("专辑艺人".into());
        t.track_number = Some(3);
        t.recording_mbid = Some("rec-mbid".into());
        let body = build_scrobble_body(
            "track.scrobble",
            &t,
            Some(1_700_000_000),
            "key",
            "sess",
            "shh",
        );
        for expect in [
            "method=track.scrobble",
            "track=%E6%9B%B2%E5%90%8D",
            "timestamp=1700000000",
            "album=%E4%B8%93%E8%BE%91",
            "albumArtist=",
            "trackNumber=3",
            "mbid=rec-mbid",
            "api_key=key",
            "sk=sess",
            "api_sig=",
            "format=json",
        ] {
            assert!(body.contains(expect), "body 缺 {expect}: {body}");
        }
    }

    /// Option 字段缺省时不传：最小载荷只剩识别核心 + 会话/签名参数。
    #[test]
    fn scrobble_body_omits_absent_optional_fields() {
        let body = build_scrobble_body(
            "track.updateNowPlaying",
            &track(),
            None,
            "key",
            "sess",
            "shh",
        );
        for absent in [
            "timestamp=",
            "album=",
            "albumArtist=",
            "trackNumber=",
            "mbid=",
        ] {
            assert!(!body.contains(absent), "body 不应含 {absent}: {body}");
        }
        // now_playing（timestamp=None）不携带时间戳参数。
        assert_eq!(
            body.split('&').find(|p| p.starts_with("method=")),
            Some("method=track.updateNowPlaying")
        );
    }

    /// 空白字符串视为缺省（Last.fm 对空串参数按非法值处理）。
    #[test]
    fn scrobble_body_treats_blank_strings_as_absent() {
        let mut t = track();
        t.album_name = Some("  ".into());
        t.recording_mbid = Some("".into());
        let body = build_scrobble_body("track.scrobble", &t, Some(1), "k", "s", "shh");
        assert!(!body.contains("album="));
        assert!(!body.contains("mbid="));
    }

    /// 网页授权：getToken 无 sk；getSession 带 token 无 sk；授权页含 api_key+token。
    #[test]
    fn web_auth_body_and_url_shape() {
        let token_body = build_get_token_body("abc", "secret");
        assert!(token_body.contains("method=auth.getToken"));
        assert!(token_body.contains("api_key=abc"));
        assert!(!token_body.contains("sk="));
        assert!(token_body.contains("api_sig="));

        let session_body = build_get_session_body("tok en", "abc", "secret");
        assert!(session_body.contains("method=auth.getSession"));
        assert!(session_body.contains("token=tok%20en"));
        assert!(!session_body.contains("sk="));

        let url = authorization_url("key/+", "tok en");
        assert!(url.starts_with("https://www.last.fm/api/auth/?"));
        assert!(url.contains("api_key=key%2F%2B"));
        assert!(url.contains("token=tok%20en"));

        assert!(is_token_not_authorized(&serde_json::json!({
            "error": 14,
            "message": "This token has not yet been authorized"
        })));
        assert!(!is_token_not_authorized(&serde_json::json!({ "error": 4 })));
        assert!(!is_token_not_authorized(
            &serde_json::json!({ "token": "x" })
        ));
    }

    // ------------------------- 多艺术家 -------------------------

    /// artist 单串口径：primary + artists 以 ", " 拼接；空白名跳过；无 artists
    /// 时退回 primary。
    #[test]
    fn join_artists_joins_with_comma_space() {
        let mut t = track();
        assert_eq!(join_artists(&t), "主艺人");
        t.artists = vec!["客串".into(), " ".into(), "另一组".into()];
        assert_eq!(join_artists(&t), "主艺人, 客串, 另一组");
    }

    // ------------------------- <30s 跳过 -------------------------

    #[test]
    fn short_tracks_are_skipped_but_unknown_duration_is_not() {
        assert!(should_skip(Some(0)));
        assert!(should_skip(Some(29)));
        assert!(!should_skip(Some(30)));
        assert!(!should_skip(Some(31)));
        assert!(!should_skip(None), "时长未知不跳过");
    }

    // ------------------------- body error 映射 -------------------------

    /// 码表逐项锁定：凭据类→InvalidArgument（不可重试）、29→RateLimited、
    /// 11/16→Network（可重试）、其余→PermanentFailure；无 error 键 → None。
    #[test]
    fn body_error_code_table() {
        fn err_of(code: i64) -> PluginError {
            body_error(&serde_json::json!({ "error": code, "message": "boom" })).unwrap()
        }
        for code in [4, 6, 9, 10, 26] {
            let e = err_of(code);
            assert_eq!(e.code, PluginErrorCode::InvalidArgument, "error {code}");
            assert!(!e.retryable, "error {code} 不可重试");
            assert!(e.message.contains(&format!("Last.fm error {code}")));
        }
        let e = err_of(29);
        assert_eq!(e.code, PluginErrorCode::RateLimited);
        assert!(e.retryable);
        for code in [11, 16] {
            let e = err_of(code);
            assert_eq!(e.code, PluginErrorCode::Network, "error {code}");
            assert!(e.retryable, "error {code} 可重试");
        }
        for code in [2, 3, 5, 7, 8, 13] {
            let e = err_of(code);
            assert_eq!(e.code, PluginErrorCode::PermanentFailure, "error {code}");
            assert!(!e.retryable);
        }
        // 无 error 键（成功响应）→ None。
        assert_eq!(body_error(&serde_json::json!({ "session": {} })), None);
        assert_eq!(body_error(&Value::Null), None);
        // message 缺省也有兜底文案。
        let e = body_error(&serde_json::json!({ "error": 4 })).unwrap();
        assert!(e.message.contains("Last.fm API error"));
        assert!(is_invalid_session(&err_of(9)));
        assert!(!is_invalid_session(&err_of(4)));
    }
}
