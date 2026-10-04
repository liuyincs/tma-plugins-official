//! listenbrainz 插件的上报纯逻辑：submit-listens 载荷组装（None 字段省略）、
//! <30s 跳过判据与 HTTP 状态 → PluginError 映射。
//!
//! 本模块**双 target 编译**（宿主 `cargo test -p tma-builtin-listenbrainz`
//! 跑单测；wasm 侧经 `crate::plugin` 引用），因此只允许纯数据变换：不声明
//! extism 宿主函数、不触碰 transport / read_config（那些只在 wasm target 存在）。
//!
//! 载荷口径（官方 JSON web service）：
//! - scrobble → `{"listen_type":"single","payload":[{"listened_at":...,
//!   "track_metadata":{...}}]}`；`duration_ms` 取标签 `duration_seconds × 1000`；
//! - now_playing → `{"listen_type":"playing_now","payload":[{"track_metadata":
//!   {...}}]}`（无 listened_at），`additional_info` 带播放器口径的
//!   `position_ms`/`duration_ms`；
//! - `track_metadata.artist_name` 是单串：primary_artist 与协作艺术家以 ", "
//!   拼接（官方对多艺术家的推荐写法；MBID 另走 `artist_mbids`）。

use serde_json::{Map, Value};
use tma_plugin_sdk::{
    PluginError, PluginErrorCode, TrackEventFields, should_skip_scrobble, status_to_error,
};

/// 过短曲目不上报（与 `tma_plugin_sdk::MIN_SCROBBLE_SECONDS` 同源）。
/// artist_name 单串口径（`tma_plugin_sdk::join_artists` 再导出，供单测）。
pub(crate) use tma_plugin_sdk::join_artists;

/// 提交种类：已完成的播放记录（single listen）或正在播放通知（playing_now）。
pub(crate) enum ListenSubmit {
    /// scrobble：`listened_at` 为播放完成时间（unix 秒），必有。
    Single { listened_at: i64 },
    /// now_playing：携带播放器口径的进度毫秒（时长缺省回退 0，见事件契约）。
    PlayingNow { position_ms: u64, duration_ms: u64 },
}

impl ListenSubmit {
    /// wire 上的 `listen_type`。
    pub(crate) fn listen_type(&self) -> &'static str {
        match self {
            Self::Single { .. } => "single",
            Self::PlayingNow { .. } => "playing_now",
        }
    }
}

/// 组装 submit-listens 请求 body（单条 payload；None/空白字段省略，
/// `additional_info` 全空时整体省略）。
pub(crate) fn build_submit_body(kind: &ListenSubmit, track: &TrackEventFields) -> String {
    let mut additional: Map<String, Value> = Map::new();
    if let Some(n) = track.track_number {
        additional.insert("track_number".into(), Value::from(n));
    }
    if let Some(mbid) = non_empty(track.album_mbid.as_deref()) {
        // 官方字段名 release_mbid（TMA 契约的 album_mbid 即上游的 release）。
        additional.insert("release_mbid".into(), Value::from(mbid));
    }
    if let Some(mbid) = non_empty(track.recording_mbid.as_deref()) {
        additional.insert("recording_mbid".into(), Value::from(mbid));
    }
    if let Some(mbids) = track.artist_mbids.as_ref().filter(|v| !v.is_empty()) {
        additional.insert(
            "artist_mbids".into(),
            Value::Array(mbids.iter().map(|m| Value::from(m.as_str())).collect()),
        );
    }
    match kind {
        ListenSubmit::Single { .. } => {
            // 标签口径秒 → 官方毫秒（saturating 防御溢出）。
            if let Some(secs) = track.duration_seconds {
                additional.insert("duration_ms".into(), Value::from(secs.saturating_mul(1000)));
            }
        }
        ListenSubmit::PlayingNow {
            position_ms,
            duration_ms,
        } => {
            additional.insert("position_ms".into(), Value::from(*position_ms));
            additional.insert("duration_ms".into(), Value::from(*duration_ms));
        }
    }

    let mut metadata: Map<String, Value> = Map::new();
    metadata.insert("track_name".into(), Value::from(track.track_title.trim()));
    metadata.insert("artist_name".into(), Value::from(join_artists(track)));
    if let Some(release) = non_empty(track.album_name.as_deref()) {
        metadata.insert("release_name".into(), Value::from(release));
    }
    if !additional.is_empty() {
        metadata.insert("additional_info".into(), Value::Object(additional));
    }

    let mut item: Map<String, Value> = Map::new();
    if let ListenSubmit::Single { listened_at } = *kind {
        item.insert("listened_at".into(), Value::from(listened_at));
    }
    item.insert("track_metadata".into(), Value::Object(metadata));

    let mut body: Map<String, Value> = Map::new();
    body.insert("listen_type".into(), Value::from(kind.listen_type()));
    body.insert("payload".into(), Value::Array(vec![Value::Object(item)]));
    Value::Object(body).to_string()
}

/// <30s 跳过判据（`tma_plugin_sdk::should_skip_scrobble` 再导出）。
pub(crate) fn should_skip(duration_seconds: Option<u64>) -> bool {
    should_skip_scrobble(duration_seconds)
}

/// HTTP 状态 → PluginError 映射（submit/validate 共用）：
///
/// - 401 → `InvalidArgument`「token 无效」（不可重试，换 token 才有解）；
/// - 400 → `PermanentFailure`（载荷被上游判定非法，重试无意义）；
/// - 其余（429/5xx/其他 3xx-4xx）走共享 [`status_to_error`：429→`RateLimited`
///   （可重试）、5xx→`Network`（可重试）、其余 4xx→`PermanentFailure`]。
pub(crate) fn status_error(status: u16, body_hint: &str) -> PluginError {
    match status {
        401 => PluginError::new(
            PluginErrorCode::InvalidArgument,
            with_hint("ListenBrainz token 无效（HTTP 401）", body_hint),
        ),
        400 => PluginError::new(
            PluginErrorCode::PermanentFailure,
            with_hint("ListenBrainz 拒绝载荷（HTTP 400）", body_hint),
        ),
        _ => status_to_error(status, body_hint),
    }
}

fn with_hint(prefix: &str, hint: &str) -> String {
    let hint = hint.trim();
    if hint.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {hint}")
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
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

    /// scrobble 全字段载荷：listen_type=single、listened_at、MB 字段进
    /// additional_info、duration_ms = 标签秒 × 1000。
    #[test]
    fn single_listen_payload_full_shape() {
        let mut t = track();
        t.artists = vec!["客串".into()];
        t.album_name = Some("专辑".into());
        t.track_number = Some(3);
        t.recording_mbid = Some("rec-mbid".into());
        t.artist_mbids = Some(vec!["artist-mbid".into()]);
        t.album_mbid = Some("release-mbid".into());
        let body = build_submit_body(
            &ListenSubmit::Single {
                listened_at: 1_735_689_600,
            },
            &t,
        );
        let expect = serde_json::json!({
            "listen_type": "single",
            "payload": [{
                "listened_at": 1735689600,
                "track_metadata": {
                    "track_name": "曲名",
                    "artist_name": "主艺人, 客串",
                    "release_name": "专辑",
                    "additional_info": {
                        "track_number": 3,
                        "release_mbid": "release-mbid",
                        "recording_mbid": "rec-mbid",
                        "artist_mbids": ["artist-mbid"],
                        "duration_ms": 213000
                    }
                }
            }]
        });
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), expect);
    }

    /// now_playing 载荷：listen_type=playing_now、无 listened_at、
    /// additional_info 带播放器口径 position_ms/duration_ms。
    #[test]
    fn playing_now_payload_shape() {
        let body = build_submit_body(
            &ListenSubmit::PlayingNow {
                position_ms: 42_000,
                duration_ms: 213_500,
            },
            &track(),
        );
        let expect = serde_json::json!({
            "listen_type": "playing_now",
            "payload": [{
                "track_metadata": {
                    "track_name": "曲名",
                    "artist_name": "主艺人",
                    "additional_info": { "duration_ms": 213500, "position_ms": 42000 }
                }
            }]
        });
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), expect);
        assert!(
            !body.contains("listened_at"),
            "playing_now 不带 listened_at"
        );
    }

    /// None 字段省略：最小载荷只剩识别核心；additional_info 全空时整体省略。
    #[test]
    fn minimal_payload_omits_absent_fields_and_empty_additional_info() {
        let mut t = track();
        t.duration_seconds = None;
        let body = build_submit_body(&ListenSubmit::Single { listened_at: 1 }, &t);
        let expect = serde_json::json!({
            "listen_type": "single",
            "payload": [{
                "listened_at": 1,
                "track_metadata": { "track_name": "曲名", "artist_name": "主艺人" }
            }]
        });
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), expect);
        assert!(!body.contains("additional_info"));
        assert!(!body.contains("release_name"));
    }

    /// 空白字符串视为缺省（album_name="  " 不产 release_name）。
    #[test]
    fn blank_strings_are_omitted() {
        let mut t = track();
        t.album_name = Some("  ".into());
        t.album_mbid = Some("".into());
        let body = build_submit_body(&ListenSubmit::Single { listened_at: 1 }, &t);
        assert!(!body.contains("release_name"));
        assert!(!body.contains("release_mbid"));
    }

    /// artist_name 单串口径：", " 拼接、空白名跳过、无协作时退回 primary。
    #[test]
    fn join_artists_joins_with_comma_space() {
        let mut t = track();
        assert_eq!(join_artists(&t), "主艺人");
        t.artists = vec!["A".into(), " ".into(), "B".into()];
        assert_eq!(join_artists(&t), "主艺人, A, B");
    }

    /// <30s 跳过：时长未知不跳过（与 lastfm 同一口径）。
    #[test]
    fn short_tracks_are_skipped_but_unknown_duration_is_not() {
        assert!(should_skip(Some(0)));
        assert!(should_skip(Some(29)));
        assert!(!should_skip(Some(30)));
        assert!(!should_skip(None));
    }

    /// 状态映射表：401→InvalidArgument（不可重试）、400→PermanentFailure、
    /// 429→RateLimited、5xx→Network（可重试）、404→PermanentFailure。
    #[test]
    fn status_error_table() {
        let e = status_error(401, "");
        assert_eq!(e.code, PluginErrorCode::InvalidArgument);
        assert!(!e.retryable, "token 无效重试无意义");
        assert!(e.message.contains("token 无效"));

        let e = status_error(400, "bad payload");
        assert_eq!(e.code, PluginErrorCode::PermanentFailure);
        assert!(!e.retryable);
        assert!(e.message.contains("bad payload"));

        let e = status_error(429, "");
        assert_eq!(e.code, PluginErrorCode::RateLimited);
        assert!(e.retryable);

        for status in [500u16, 502, 503] {
            let e = status_error(status, "");
            assert_eq!(e.code, PluginErrorCode::Network, "HTTP {status}");
            assert!(e.retryable, "HTTP {status} 可重试");
        }

        let e = status_error(404, "");
        assert_eq!(e.code, PluginErrorCode::PermanentFailure);
    }
}
