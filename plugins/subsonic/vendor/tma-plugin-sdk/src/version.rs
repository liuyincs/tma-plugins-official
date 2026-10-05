//! Canonical plugin version parsing and ordering.
//!
//! Plugin manifests cross several storage and runtime boundaries. Keep their
//! version semantics here instead of teaching each caller a different subset
//! of SemVer.

use std::cmp::Ordering;

pub use semver::{BuildMetadata, Prerelease, Version};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid semantic version {value:?}: {message}")]
pub struct VersionError {
    pub value: String,
    pub message: String,
}

pub fn parse_version(value: &str) -> Result<Version, VersionError> {
    Version::parse(value).map_err(|source| VersionError {
        value: value.to_owned(),
        message: source.to_string(),
    })
}

pub fn compare_versions(left: &str, right: &str) -> Result<Ordering, VersionError> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_is_lower_than_release() {
        assert_eq!(compare_versions("2.0.0-rc.1", "2.0.0"), Ok(Ordering::Less));
        assert_eq!(
            compare_versions("2.0.0-rc.2", "2.0.0-rc.10"),
            Ok(Ordering::Less)
        );
        assert!(compare_versions("nope", "2.0.0").is_err());
    }
}
