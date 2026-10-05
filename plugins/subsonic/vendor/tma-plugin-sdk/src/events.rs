//! 事件订阅扩展点 wire DTO（ABI 1.3 起）：宿主 → 插件的播放事件推送。
//!
//! 形态与 [`crate::message`] 的 `tma_action` 同构：服务端 ↔ 插件直接
//! JSON in/out（scrobble 带稳定 event_id，instance-per-call 单飞行）。请求是内部标签枚举
//! （`{"kind":"scrobble", ...载荷字段}`），kind 即事件种类的单一事实来源。
//!
//! 事件语义：
//! - **scrobble**：一次已完成的播放记录（含 `played_at` 时间戳），接收方落库；
//! - **now_playing**：即时"正在播放"通知（含播放进度），接收方仅展示，不落库。
//!
//! 插件订阅哪些事件由 manifest `scrobble_reporter.events` 声明（见
//! [`crate::manifest::ScrobbleReporterManifest`]），宿主只推送已订阅种类。

use serde::{Deserialize, Serialize};

use crate::library_type::LibraryType;
use crate::message::PluginError;

fn default_library_type() -> LibraryType {
    LibraryType::Music
}

// ---------------------------------------------------------------------------
// 请求 DTO
// ---------------------------------------------------------------------------

/// 事件种类（wire 名即 manifest `scrobble_reporter.events` 的合法取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEventKind {
    /// 已完成的播放记录（提交点语义，含 `played_at`）。
    Scrobble,
    /// 即时"正在播放"通知（含进度，不落库）。
    NowPlaying,
}

/// `tma_event` 可选导出的请求（独立于 [`crate::message::PluginOp`] scrape
/// 管线，ABI 1.3 起）。
///
/// 线形态（内部标签 + snake_case）：
/// `{"kind":"scrobble","track_title":"...","primary_artist":"...","played_at":...}`
/// 或 `{"kind":"now_playing",...,"position_ms":...,"duration_ms":...}`——
/// kind 与载荷变体一一对应，不存在冗余的双重编码。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEventRequest {
    /// 播放记录事件（载荷 [`ScrobbleEventPayload`]）。
    Scrobble(ScrobbleEventPayload),
    /// 正在播放事件（载荷 [`NowPlayingEventPayload`]）。
    NowPlaying(NowPlayingEventPayload),
}

impl PluginEventRequest {
    /// 事件种类（宿主侧路由/插件侧分发共用）。
    pub fn kind(&self) -> PluginEventKind {
        match self {
            Self::Scrobble(_) => PluginEventKind::Scrobble,
            Self::NowPlaying(_) => PluginEventKind::NowPlaying,
        }
    }

    /// 曲目字段视图：两种事件的共同载荷（事件特有字段需按变体匹配取用）。
    pub fn track(&self) -> &TrackEventFields {
        match self {
            Self::Scrobble(payload) => &payload.track,
            Self::NowPlaying(payload) => &payload.track,
        }
    }
}

/// 两种播放事件共享的曲目字段（事件特有字段在各自载荷上）。
///
/// 识别核心（曲名/主艺术家）必有；专辑与编号等元数据缺失即 `null`；
/// MusicBrainz 标识全部可缺（本地标签库不含 MBID 是常态，插件按名称匹配回退）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackEventFields {
    /// 曲目当前所属资料库类型（ABI 1.5 起；上报/now_playing 闸门依据）。
    /// 缺省 `music`：升级前入队的旧 outbox 载荷不含该键，按 music 继续投递。
    #[serde(default = "default_library_type")]
    pub library_type: LibraryType,
    /// 曲目标题（识别核心之一）。
    pub track_title: String,
    /// 主艺术家名（识别核心之一；协作艺术家见 `artists`）。
    pub primary_artist: String,
    /// 协作/客串艺术家名列表（不含 primary_artist；无则为空）。
    #[serde(default)]
    pub artists: Vec<String>,
    /// 专辑标题。
    pub album_name: Option<String>,
    /// 专辑艺术家名（合辑等场景可能与 primary_artist 不同）。
    pub album_artist: Option<String>,
    /// 曲目在专辑内的序号（1 起）。
    pub track_number: Option<u32>,
    /// 碟片序号（1 起）。
    pub disc_number: Option<u32>,
    /// 首发年份。
    pub year: Option<i32>,
    /// 曲目时长（秒；与 now_playing 的 `duration_ms` 口径不同，单位以此处为准）。
    pub duration_seconds: Option<u64>,
    /// recording MBID（曲目级）。
    pub recording_mbid: Option<String>,
    /// 艺术家 MBID 列表（与 primary_artist/artists 对应关系由插件自行匹配）。
    pub artist_mbids: Option<Vec<String>>,
    /// album（release）MBID。契约统一用 TMA 的「专辑」术语命名，
    /// 上游 ListenBrainz 的 `release_mbid` 即此字段。
    pub album_mbid: Option<String>,
}

/// scrobble 事件载荷：一次已完成的播放记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrobbleEventPayload {
    /// Stable id for at-least-once delivery deduplication. This is the
    /// catalog scrobble id and remains unchanged across retries.
    pub event_id: String,
    /// 曲目字段（与 now_playing 共享形态）。
    #[serde(flatten)]
    pub track: TrackEventFields,
    /// 播放完成时间（unix 秒）。scrobble 的时间戳是去重与排序的依据，必有。
    pub played_at: i64,
}

/// now_playing 事件载荷：即时"正在播放"通知（进度毫秒口径，与播放器上报一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NowPlayingEventPayload {
    /// 曲目字段（与 scrobble 共享形态）。
    #[serde(flatten)]
    pub track: TrackEventFields,
    /// 当前播放位置（毫秒）。
    pub position_ms: u64,
    /// 曲目总时长（毫秒；播放器侧口径，可能与标签 `duration_seconds` 有毫秒级出入）。
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

/// `tma_event` 可选导出的响应：
/// `{"ok": bool, "message": string|null, "error": {code,message,retryable}|null}`。
///
/// 成败表达与 [`crate::message::PluginActionResponse`] 同构（`ok` 布尔 +
/// 人类可读 `message`）；区别在失败细节复用 [`PluginError`] 纯数据错误
/// （`code`/`retryable` 可编程判别）——scrobble 上游限流/网络抖动是常态，
/// 宿主需按 `retryable` 决定是否入重试队列，不能只靠文案。
///
/// `persist_secrets` 与动作响应对齐：插件可在事件处理中换发令牌并交宿主
/// 加密写回该用户配置（如 Last.fm session_key 失效后静默重换），不必等用户
/// 再保存表单。白名单规则同动作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEventResponse {
    pub ok: bool,
    /// 人类可读确认/失败文案（null = 无）。
    #[serde(default)]
    pub message: Option<String>,
    /// 失败详情（仅 `ok:false` 时给；`ok:true` 恒为 null）。
    #[serde(default)]
    pub error: Option<PluginError>,
    /// 宿主落盘密钥（与 [`crate::message::PluginActionResponse::persist_secrets`]
    /// 同语义）。缺省省略，旧插件响应零变化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_secrets: Option<std::collections::BTreeMap<String, String>>,
}

impl PluginEventResponse {
    /// 成功响应（无文案）。
    pub fn accepted() -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            persist_secrets: None,
        }
    }

    /// 成功响应（带确认文案）。
    pub fn accepted_with_message(message: String) -> Self {
        Self {
            ok: true,
            message: Some(message),
            error: None,
            persist_secrets: None,
        }
    }

    /// 失败响应：`message` 取错误文案，`error` 携带可编程判别细节。
    pub fn rejected(error: PluginError) -> Self {
        Self {
            ok: false,
            message: Some(error.message.clone()),
            error: Some(error),
            persist_secrets: None,
        }
    }

    /// 附带 persist_secrets（事件中途换发的令牌）。空表视为无。
    pub fn with_persist_secrets(
        mut self,
        persist: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.persist_secrets = if persist.is_empty() {
            None
        } else {
            Some(persist)
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PluginErrorCode;

    fn track_fields() -> TrackEventFields {
        TrackEventFields {
            library_type: LibraryType::Music,
            track_title: "曲名".into(),
            primary_artist: "艺人".into(),
            artists: vec!["客串艺人".into()],
            album_name: Some("专辑".into()),
            album_artist: Some("专辑艺人".into()),
            track_number: Some(3),
            disc_number: Some(1),
            year: Some(2024),
            duration_seconds: Some(213),
            recording_mbid: Some("rec-mbid".into()),
            artist_mbids: Some(vec!["artist-mbid".into()]),
            album_mbid: Some("album-mbid".into()),
        }
    }

    /// scrobble 信封：线形态为内部标签 + 扁平载荷字段（kind 单一事实来源）。
    #[test]
    fn scrobble_request_serde_shape() {
        let req = PluginEventRequest::Scrobble(ScrobbleEventPayload {
            event_id: "scrobble-1".into(),
            track: track_fields(),
            played_at: 1_735_689_600,
        });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"scrobble","event_id":"scrobble-1","library_type":"music","track_title":"曲名","primary_artist":"艺人","artists":["客串艺人"],"album_name":"专辑","album_artist":"专辑艺人","track_number":3,"disc_number":1,"year":2024,"duration_seconds":213,"recording_mbid":"rec-mbid","artist_mbids":["artist-mbid"],"album_mbid":"album-mbid","played_at":1735689600}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginEventRequest>(&json).unwrap(),
            req
        );
        assert_eq!(req.kind(), PluginEventKind::Scrobble);
    }

    /// now_playing 信封：同曲目字段 + 毫秒进度口径。
    #[test]
    fn now_playing_request_serde_shape() {
        let req = PluginEventRequest::NowPlaying(NowPlayingEventPayload {
            track: track_fields(),
            position_ms: 42_000,
            duration_ms: 213_000,
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.starts_with(
                r#"{"kind":"now_playing","library_type":"music","track_title":"曲名""#
            )
        );
        // now_playing 不携带 scrobble 特有的 played_at 字段。
        assert!(!json.contains("played_at"));
        assert_eq!(
            serde_json::from_str::<PluginEventRequest>(&json).unwrap(),
            req
        );
        assert_eq!(req.kind(), PluginEventKind::NowPlaying);
    }

    /// 最小事件载荷：除识别核心外全部可缺（缺省键解析为 None/空表）。
    #[test]
    fn minimal_payload_parses_with_defaults() {
        let req: PluginEventRequest = serde_json::from_str(
            r#"{"kind":"scrobble","event_id":"e-1","track_title":"T","primary_artist":"A","played_at":1}"#,
        )
        .unwrap();
        let PluginEventRequest::Scrobble(payload) = &req else {
            panic!("kind 必须反序列化回 scrobble");
        };
        assert_eq!(payload.played_at, 1);
        assert_eq!(payload.track.artists, Vec::<String>::new());
        assert_eq!(payload.track.album_name, None);
        assert_eq!(payload.track.recording_mbid, None);
        assert_eq!(payload.track.artist_mbids, None);

        let np: PluginEventRequest = serde_json::from_str(
            r#"{"kind":"now_playing","track_title":"T","primary_artist":"A","position_ms":0,"duration_ms":1}"#,
        )
        .unwrap();
        let PluginEventRequest::NowPlaying(payload) = &np else {
            panic!("kind 必须反序列化回 now_playing");
        };
        assert_eq!(payload.position_ms, 0);
        assert_eq!(payload.duration_ms, 1);
    }

    /// 事件种类 wire 名与 manifest `scrobble_reporter.events` 合法值逐字一致。
    #[test]
    fn event_kind_wire_names() {
        assert_eq!(
            serde_json::to_string(&PluginEventKind::Scrobble).unwrap(),
            r#""scrobble""#
        );
        assert_eq!(
            serde_json::to_string(&PluginEventKind::NowPlaying).unwrap(),
            r#""now_playing""#
        );
        // 未知事件种类在前向兼容上按解析失败处理（新种类属 minor ABI 变更）。
        assert!(serde_json::from_str::<PluginEventKind>(r#""listen""#).is_err());
    }

    /// 事件响应信封：成功无文案/带文案、失败携带 PluginError 细节。
    #[test]
    fn event_response_serde_shape() {
        let accepted = PluginEventResponse::accepted();
        let json = serde_json::to_string(&accepted).unwrap();
        assert_eq!(json, r#"{"ok":true,"message":null,"error":null}"#);
        assert_eq!(
            serde_json::from_str::<PluginEventResponse>(&json).unwrap(),
            accepted
        );

        let with_message = PluginEventResponse::accepted_with_message("已记录".into());
        assert_eq!(
            serde_json::to_string(&with_message).unwrap(),
            r#"{"ok":true,"message":"已记录","error":null}"#
        );

        let rejected = PluginEventResponse::rejected(PluginError::new(
            PluginErrorCode::RateLimited,
            "upstream 429".into(),
        ));
        let json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(
            json,
            r#"{"ok":false,"message":"upstream 429","error":{"code":"rate_limited","message":"upstream 429","retryable":true}}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginEventResponse>(&json).unwrap(),
            rejected
        );
        assert!(rejected.error.unwrap().retryable);
    }

    #[test]
    fn event_response_persist_secrets_roundtrip() {
        let mut persist = std::collections::BTreeMap::new();
        persist.insert("session_key".into(), "sk-new".into());
        let resp = PluginEventResponse::accepted().with_persist_secrets(persist.clone());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("session_key"));
        assert_eq!(
            serde_json::from_str::<PluginEventResponse>(&json).unwrap(),
            resp
        );
        assert_eq!(
            PluginEventResponse::accepted()
                .with_persist_secrets(Default::default())
                .persist_secrets,
            None
        );
    }
}
