//! 歌单导入扩展点（`playlist_import`，ABI 1.4+）的 wire DTO。
//!
//! 形态对齐既有扩展点契约（[`crate::scrape`] / [`crate::events`]）：这里是
//! 纯数据线协议，不含宿主 pipeline 专用字段。宿主拿到插件输出后自行做曲目
//! 匹配、工作台落库与草稿管理——本 DTO 不承诺也不描述这些下游行为。
//!
//! 与宿主侧已解析曲目类型（`ParsedTrack`）的映射约定（后续 ABI 转换
//! 时照此执行，避免两边各写一套猜测）：
//!
//! | 本 DTO（[`PlaylistImportTrack`]） | `ParsedTrack` |
//! | --- | --- |
//! | `title` | `title`（原样；空串照搬，匹配阶段自行降级） |
//! | `artists`（显式列表） | `artist: Option<String>`，按 `", "` 连接；空列表 → `None` |
//! | `album` | `album`（原样） |
//!
//! 连接分隔符取 `", "`，见 [`join_artist_names`]（事件侧
//! [`crate::guest::join_artists`] 复用同一分隔符口径）。

use serde::{Deserialize, Serialize};

/// 将艺术家名列表用 `", "` 连接；逐段 trim、去空，空列表 → `None`。
///
/// 宿主把 [`PlaylistImportTrack::artists`] 映射到 `ParsedTrack.artist` 时照此执行；
/// [`crate::guest::join_artists`] 对事件侧字段合串时复用同一分隔符。
pub fn join_artist_names(artists: impl IntoIterator<Item = impl AsRef<str>>) -> Option<String> {
    let joined = artists
        .into_iter()
        .map(|s| s.as_ref().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    (!joined.is_empty()).then_some(joined)
}

/// 宿主→插件的单次歌单导入调用（`tma_playlist_import` 导出函数的入参）。
///
/// `method_id` 是清单 `playlist_import.methods[].id`，宿主按它选择调用语义；
/// 同一插件可声明多个方法（如 `from_text` / `from_image`），DTO 形态复用同一
/// 条通道，方法差异只体现在 `input` 的自由 JSON 上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistImportRequest {
    /// 目标方法 id（清单已声明且校验过字符集）。
    pub method_id: String,
    /// 方法输入（清单 `input_schema` 描述的自由 JSON；未声明 schema 时为空 object）。
    #[serde(default = "empty_object")]
    pub input: serde_json::Value,
    /// 该插件已注入的**每用户配置**（`user_config_schema` 对应的已解密值；
    /// 敏感字段宿主侧已按 `user_config_secrets` 白名单解出，插件无需再询问）。
    #[serde(default = "empty_object")]
    pub user_config: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// 插件→宿主的歌单导入结果（`tma_playlist_import` 出参）。
///
/// 插件只负责"把非结构化来源变成曲目候选清单"；`name`/`description` 是插件
/// 的**建议**值，宿主可采纳、可让用户改写后落库（宿主不采纳也不算错误）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistImportResponse {
    /// 解析出的曲目候选（保序；重复项由宿主侧去重，插件无需保证唯一）。
    pub tracks: Vec<PlaylistImportTrack>,
    /// 插件建议的歌单名（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 插件建议的歌单描述（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 单条曲目候选。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlaylistImportTrack {
    /// 曲名（必填；来源缺失时插件应给空串而不是丢弃该行，便于用户在工作台补齐）。
    #[serde(default)]
    pub title: String,
    /// 艺术家列表（显式多艺术家；缺省 = 无艺术家。映射到 `ParsedTrack.artist`
    /// 时按 `", "` 连接，见模块级文档）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<String>,
    /// 专辑名（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_shape() {
        let req = PlaylistImportRequest {
            method_id: "from_text".into(),
            input: serde_json::json!({"text": "1. Song - Artist"}),
            user_config: serde_json::json!({"language": "zh"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"method_id":"from_text","input":{"text":"1. Song - Artist"},"user_config":{"language":"zh"}}"#
        );
        assert_eq!(
            serde_json::from_str::<PlaylistImportRequest>(&json).unwrap(),
            req
        );
    }

    /// input / user_config 缺省为空 object（宽松解析：宿主可只带 method_id）。
    #[test]
    fn request_defaults_input_and_user_config_to_empty_object() {
        let req: PlaylistImportRequest =
            serde_json::from_str(r#"{"method_id":"from_text"}"#).unwrap();
        assert_eq!(req.input, serde_json::json!({}));
        assert_eq!(req.user_config, serde_json::json!({}));
    }

    /// input 是自由 JSON：数组、标量等任意形态都放行（结构约束在清单 schema 层）。
    #[test]
    fn request_accepts_free_form_input() {
        for input in [
            serde_json::json!(null),
            serde_json::json!([1, 2, 3]),
            serde_json::json!("raw"),
            serde_json::json!({"nested": {"deep": [true, 1.5]}}),
        ] {
            let req = PlaylistImportRequest {
                method_id: "m".into(),
                input: input.clone(),
                user_config: serde_json::json!({}),
            };
            let json = serde_json::to_string(&req).unwrap();
            assert_eq!(
                serde_json::from_str::<PlaylistImportRequest>(&json)
                    .unwrap()
                    .input,
                input
            );
        }
    }

    #[test]
    fn response_serde_shape() {
        let resp = PlaylistImportResponse {
            tracks: vec![
                PlaylistImportTrack {
                    title: "Song".into(),
                    artists: vec!["A".into(), "B".into()],
                    album: Some("Album".into()),
                },
                PlaylistImportTrack {
                    title: "No metadata".into(),
                    artists: vec![],
                    album: None,
                },
            ],
            name: Some("从文本导入".into()),
            description: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"tracks":[{"title":"Song","artists":["A","B"],"album":"Album"},{"title":"No metadata"}],"name":"从文本导入"}"#
        );
        assert_eq!(
            serde_json::from_str::<PlaylistImportResponse>(&json).unwrap(),
            resp
        );
    }

    /// `ParsedTrack` 映射约定：artists 经 [`join_artist_names`] 连接，空列表 → None。
    #[test]
    fn join_artist_names_matches_parsed_track_convention() {
        assert_eq!(
            join_artist_names(["A", "B"]),
            Some("A, B".to_string()),
            "分隔符须与 guest::join_artists 及库内逗号 joined 口径一致"
        );
        assert_eq!(join_artist_names(Vec::<&str>::new()), None);
        assert_eq!(
            join_artist_names(["  A  ", "", "B "]),
            Some("A, B".to_string())
        );
    }

    /// 无建议名/描述时省略键（线形态最小）；`tracks` 恒出现（可为空数组）。
    #[test]
    fn response_omits_absent_suggestions_and_keeps_tracks_key() {
        let resp = PlaylistImportResponse {
            tracks: vec![],
            name: None,
            description: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"tracks":[]}"#);
    }
}
