//! 刮削扩展点 wire DTO：跨插件边界可传的纯数据形状。
//!
//! 宿主运行时 DTO（`EntityQuery`/`ProviderResult` 等）在宿主侧 provider 层；
//! 此处仅定义过线形状。未来 WASM 边界由 scrape 做投影。

use serde::{Deserialize, Serialize};

use crate::library_type::LibraryType;

// ---------------------------------------------------------------------------
// 实体标识（wire）
// ---------------------------------------------------------------------------

/// 刮削查询实体类型（wire；落库列枚举见宿主存储层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrapeEntityKind {
    Artist,
    Album,
    Track,
}

/// 刮削能力单项（wire；勿与 `tma_core::Capability` 权限能力混淆）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrapeCapability {
    /// 写入外部 ID（`entity_external_ids`）。
    ExternalIds,
    /// 艺术家简介。
    Bio,
    /// 艺术家主图。
    Image,
}

// ---------------------------------------------------------------------------
// 请求 DTO
// ---------------------------------------------------------------------------

/// 单条外部 ID（写入 `entity_external_ids`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchedId {
    pub provider: String,
    pub external_id: String,
    pub url: Option<String>,
}

/// 刮削查询参数（wire）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityQuery {
    pub kind: ScrapeEntityKind,
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    /// 实体主名称：艺术家名或专辑标题。
    pub name: Option<String>,
    /// 专辑艺术家显示名（仅 kind=Album 时使用）。
    pub artist_name: Option<String>,
    /// 已知外部 ID（供后续来源复用，如 Wikipedia 直接用 wikidata QID）。
    pub known_external_ids: Vec<FetchedId>,
    /// 本次刮削相关的资料库类型 = 实体相关类型 ∩ 本插件声明（ABI 1.5 起）。
    ///
    /// 专辑恒 1 个元素；艺术家可多个（跨库并集）。只声明 `music` 的插件
    /// 不会在数组里看到 `audiobook`/`podcast`。
    #[serde(default)]
    pub library_types: Vec<LibraryType>,
}

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

/// 远程图片；缺 license/attribution 时不落图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteImage {
    pub url: String,
    pub license: Option<String>,
    pub attribution: Option<String>,
    /// 来源 slug（与 `Bio.source` 对称）：`fanart` / `lastfm` / `wikimedia_commons`。
    pub source: String,
}

/// Fanart.tv 等多套图条目（`artist_images` 表一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteExtraImage {
    /// `thumb` / `artistbackground` / `musiclogo` / `musicbanner`
    pub kind: String,
    pub url: String,
    /// 来源标签，如 `fanart`。
    pub source: String,
}

/// 艺术家简介（含来源标签，如 `wikipedia_zh-hans`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bio {
    pub text: String,
    pub source: String,
}

/// 刮削到的艺术家别名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrapedAlias {
    pub name: String,
    pub source: String,
    pub locale: Option<String>,
    pub primary: bool,
}

/// MusicBrainz release-group browse 落库的单条官方碟谱。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrapedOfficialAlbum {
    pub rg_mbid: String,
    pub title: String,
    pub title_norm: String,
    pub year: Option<i32>,
    /// 小写：`album` | `ep`
    pub primary_type: String,
}

/// MusicBrainz release 下的单条曲目（仅 MB 专辑刮削填充）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrapedRecording {
    pub disc_number: i32,
    pub track_number: i32,
    pub title: String,
    pub length_ms: Option<i32>,
    pub recording_mbid: Option<String>,
    pub track_mbid: Option<String>,
    pub aliases: Vec<ScrapedAlias>,
}

/// 单来源一次刮削结果（wire；不含宿主 pipeline 专用字段）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrapeResult {
    pub external_ids: Vec<FetchedId>,
    pub bio: Option<Bio>,
    pub image: Option<RemoteImage>,
    pub extra_images: Vec<RemoteExtraImage>,
    pub aliases: Vec<ScrapedAlias>,
    /// None = 未拉取；Some = 已拉取（允许空表整表替换）。
    pub tracklist: Option<Vec<ScrapedRecording>>,
    /// 置信度：MBID/ISRC≈1.0；模糊 <1.0。
    pub confidence: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
        let json = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, v, "roundtrip 失败：{json}");
    }

    #[test]
    fn scrape_entity_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ScrapeEntityKind::Artist).unwrap(),
            r#""artist""#
        );
        roundtrip(&ScrapeEntityKind::Album);
        roundtrip(&ScrapeEntityKind::Track);
    }

    #[test]
    fn scrape_capability_serializes_as_string_array_element() {
        let caps = [ScrapeCapability::ExternalIds, ScrapeCapability::Bio];
        assert_eq!(
            serde_json::to_string(&caps).unwrap(),
            r#"["external_ids","bio"]"#
        );
        roundtrip(&ScrapeCapability::Image);
    }

    #[test]
    fn entity_query_serde_roundtrip() {
        let q = EntityQuery {
            kind: ScrapeEntityKind::Album,
            mbid: Some("mbid-1".into()),
            isrc: None,
            name: Some("专辑".into()),
            artist_name: Some("艺术家".into()),
            known_external_ids: vec![FetchedId {
                provider: "wikidata".into(),
                external_id: "Q42".into(),
                url: None,
            }],
            library_types: vec![LibraryType::Music],
        };
        let json = serde_json::to_string(&q).unwrap();
        assert!(json.contains(r#""kind":"album""#));
        roundtrip(&q);
    }

    #[test]
    fn scrape_result_serde_roundtrip() {
        let result = ScrapeResult {
            external_ids: vec![FetchedId {
                provider: "musicbrainz".into(),
                external_id: "mbid-1".into(),
                url: None,
            }],
            bio: Some(Bio {
                text: "简介".into(),
                source: "wikipedia_zh-hans".into(),
            }),
            image: Some(RemoteImage {
                url: "https://example.com/a.jpg".into(),
                license: Some("CC BY-SA 4.0".into()),
                attribution: Some("作者".into()),
                source: "wikimedia_commons".into(),
            }),
            extra_images: vec![RemoteExtraImage {
                kind: "thumb".into(),
                url: "https://example.com/t.jpg".into(),
                source: "fanart".into(),
            }],
            aliases: vec![ScrapedAlias {
                name: "别名".into(),
                source: "musicbrainz".into(),
                locale: Some("zh".into()),
                primary: false,
            }],
            tracklist: Some(vec![ScrapedRecording {
                disc_number: 1,
                track_number: 2,
                title: "曲目".into(),
                length_ms: Some(180_000),
                recording_mbid: Some("r1".into()),
                track_mbid: None,
                aliases: Vec::new(),
            }]),
            confidence: 0.95,
        };
        roundtrip(&result);
        assert_eq!(ScrapeResult::default().confidence, 0.0);
    }

    #[test]
    fn scraped_official_album_roundtrip() {
        roundtrip(&ScrapedOfficialAlbum {
            rg_mbid: "rg-1".into(),
            title: "碟".into(),
            title_norm: "die".into(),
            year: Some(1999),
            primary_type: "album".into(),
        });
    }
}
