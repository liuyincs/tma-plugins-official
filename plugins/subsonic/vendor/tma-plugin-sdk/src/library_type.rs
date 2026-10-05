//! 资料库类型 wire 闭集（ABI 1.5 起）：插件清单可声明的支持范围。
//!
//! 权威业务枚举在宿主侧资料库类型（DB CHECK 亦以
//! `music` / `audiobook` / `podcast` 为闭集）。本 crate 不能依赖 storage
//! （分层），因此这里只镜像同名闭集；两侧的显式换算在宿主边界用
//! `as_str` + `FromStr` 收口，任何一侧新增变体都会先在那里编译失败。

use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// 资料库类型（wire；与宿主侧资料库类型同名闭集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryType {
    Music,
    Audiobook,
    Podcast,
}

impl LibraryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Audiobook => "audiobook",
            Self::Podcast => "podcast",
        }
    }
}

impl FromStr for LibraryType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "music" => Ok(Self::Music),
            "audiobook" => Ok(Self::Audiobook),
            "podcast" => Ok(Self::Podcast),
            other => Err(format!("未知资料库类型: {other}")),
        }
    }
}

/// 未声明 `library_types` 时的缺省支持范围。
pub const DEFAULT_LIBRARY_TYPES: [LibraryType; 1] = [LibraryType::Music];

/// 解析清单段 `library_types`：`None` → [`DEFAULT_LIBRARY_TYPES`]。
pub fn resolve_library_types(declared: Option<&[LibraryType]>) -> &[LibraryType] {
    match declared {
        Some(types) => types,
        None => &DEFAULT_LIBRARY_TYPES,
    }
}

/// 声明是否覆盖 `wanted` 中任一类型；`wanted` 为空 → `false`。
pub fn covers(declared: &[LibraryType], wanted: &[LibraryType]) -> bool {
    if wanted.is_empty() {
        return false;
    }
    wanted.iter().any(|ty| declared.contains(ty))
}

/// 声明是否覆盖单个类型。
pub fn covers_one(declared: &[LibraryType], wanted: LibraryType) -> bool {
    declared.contains(&wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_lowercase() {
        assert_eq!(
            serde_json::to_string(&LibraryType::Music).unwrap(),
            r#""music""#
        );
        assert_eq!(
            serde_json::to_string(&LibraryType::Audiobook).unwrap(),
            r#""audiobook""#
        );
        assert_eq!(
            serde_json::to_string(&LibraryType::Podcast).unwrap(),
            r#""podcast""#
        );
        assert_eq!(LibraryType::Podcast.as_str(), "podcast");
    }

    #[test]
    fn unknown_value_is_rejected() {
        assert!(serde_json::from_str::<LibraryType>(r#""video""#).is_err());
        assert!("video".parse::<LibraryType>().is_err());
    }

    #[test]
    fn default_is_music() {
        assert_eq!(DEFAULT_LIBRARY_TYPES, [LibraryType::Music]);
        assert_eq!(resolve_library_types(None), &[LibraryType::Music]);
    }

    #[test]
    fn covers_empty_wanted_is_false() {
        assert!(!covers(&[LibraryType::Music], &[]));
    }

    #[test]
    fn covers_any_overlap() {
        assert!(covers(
            &[LibraryType::Music],
            &[LibraryType::Music, LibraryType::Podcast]
        ));
        assert!(!covers(&[LibraryType::Music], &[LibraryType::Audiobook]));
        assert!(covers_one(&[LibraryType::Podcast], LibraryType::Podcast));
    }
}
