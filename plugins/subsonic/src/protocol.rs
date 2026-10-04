#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use tma_plugin_sdk::{MediaByteRange, PluginError, PluginErrorCode, PluginHttpRequest};

pub(crate) const VERSION: &str = "1.16.1";

// 认证由宿主完成：apiKey / t / s / enc: 在进入插件前已剥离，u 照常转发。
const COMMON_PARAMS: &[&str] = &["c", "v", "f", "u", "p"];

/// `v` 是客户端声明的协议版本：服务端实现 1.16.1，按 Subsonic 惯例接受
/// 任何不高于该版本的声明（客户端按服务端返回的 version 字段对齐行为），
/// 更高的版本无法保证向后兼容才拒绝。
fn supported_version(raw: &str) -> bool {
    let mut parts = raw.split('.').map(|part| part.parse::<u16>());
    let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) else {
        return false;
    };
    let patch = parts.next().unwrap_or(Ok(0));
    match patch {
        Ok(patch) if parts.next().is_none() => (major, minor, patch) <= (1, 16, 1),
        _ => false,
    }
}

pub(crate) fn validate_protocol(request: &PluginHttpRequest) -> Result<(), PluginError> {
    let version = param(request, "v").ok_or_else(|| protocol_error(10, "缺少参数 v"))?;
    if !supported_version(&version) {
        return Err(protocol_error(0, "不支持的 Subsonic 协议版本"));
    }

    let endpoint = endpoint_name(&request.path);
    let endpoint_params: &[&str] = match endpoint {
        "ping" | "getMusicFolders" => &[],
        "getArtists" => &[
            "name",
            "query",
            "musicFolderId",
            "count",
            "offset",
            "sortBy",
            "order",
        ],
        "getAlbum" | "getSong" => &["id"],
        "getArtist" => &["id", "includeNotPresent"],
        "getAlbumList" | "getAlbumList2" => &[
            "type",
            "size",
            "offset",
            "fromYear",
            "toYear",
            "genre",
            "musicFolderId",
        ],
        "getUser" => &["username"],
        "getCoverArt" => &["id", "size"],
        "stream" => &["id", "format", "maxBitRate", "estimateContentLength"],
        "star" | "unstar" | "setRating" | "scrobble" | "search3" | "getPlaylists" => &[],
        _ => &[],
    };
    for name in request.query.keys() {
        if !COMMON_PARAMS.contains(&name.as_str()) && !endpoint_params.contains(&name.as_str()) {
            return Err(protocol_error(0, &format!("不支持的参数 {name}")));
        }
    }
    Ok(())
}

/// Return the Subsonic endpoint name from a routed `/rest/...` path.
///
/// Subsonic clients conventionally append `.view`; accepting both spellings
/// keeps the HTTP route generic while making the protocol boundary explicit.
pub(crate) fn endpoint_name(path: &str) -> &str {
    let endpoint = path
        .rsplit('/')
        .find(|value| !value.is_empty())
        .unwrap_or("ping");
    endpoint.strip_suffix(".view").unwrap_or(endpoint)
}

/// Parse the one-range form supported by the host media DTO.
pub(crate) fn parse_range(value: &str) -> Result<MediaByteRange, PluginError> {
    let value = value
        .strip_prefix("bytes=")
        .ok_or_else(|| protocol_error(10, "Range 必须使用 bytes= 前缀"))?;
    if value.contains(',') {
        return Err(protocol_error(10, "暂不支持多段 Range"));
    }
    let (start, end) = value
        .trim()
        .split_once('-')
        .ok_or_else(|| protocol_error(10, "Range 格式无效"))?;
    if start.is_empty() {
        return Err(protocol_error(10, "暂不支持后缀 Range"));
    }
    let start = start
        .parse()
        .map_err(|_| protocol_error(10, "Range 起始位置无效"))?;
    let end = (!end.is_empty())
        .then(|| end.parse())
        .transpose()
        .map_err(|_| protocol_error(10, "Range 结束位置无效"))?;
    if end.is_some_and(|end| end < start) {
        return Err(protocol_error(10, "Range 结束位置早于起始位置"));
    }
    Ok(MediaByteRange { start, end })
}

pub(crate) fn param(request: &PluginHttpRequest, name: &str) -> Option<String> {
    request
        .query
        .get(name)
        .and_then(|values| values.first())
        .cloned()
}

pub(crate) fn protocol_error(code: u16, message: &str) -> PluginError {
    PluginError::new(
        PluginErrorCode::PermanentFailure,
        format!("Subsonic error {code}: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn request(query: &[(&str, &str)]) -> PluginHttpRequest {
        PluginHttpRequest {
            method: "GET".into(),
            path: "/rest/ping.view".into(),
            query: query
                .iter()
                .map(|(name, value)| ((*name).into(), vec![(*value).into()]))
                .collect::<BTreeMap<_, _>>(),
            headers: BTreeMap::new(),
            body_b64: None,
            identity: None,
        }
    }

    #[test]
    fn accepts_version_at_or_below_supported() {
        for version in [VERSION, "1.16.0", "1.13.0", "1.8.0"] {
            assert!(
                validate_protocol(&request(&[("v", version)])).is_ok(),
                "{version} must be accepted"
            );
        }
    }

    #[test]
    fn rejects_missing_or_newer_version() {
        assert!(validate_protocol(&request(&[])).is_err());
        for version in ["1.17.0", "2.0.0", "1.16.2", "abc"] {
            assert!(
                validate_protocol(&request(&[("v", version)])).is_err(),
                "{version} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_unknown_query_parameters() {
        let error = validate_protocol(&request(&[("v", VERSION), ("unexpected", "1")]))
            .expect_err("unknown parameters must be rejected");
        assert!(error.message.contains("unexpected"));
    }

    #[test]
    fn accepts_standard_view_suffix_and_rejects_unsupported_auth_parameters() {
        let mut view_request = request(&[("v", VERSION)]);
        view_request.path = "/rest/getArtists.view".into();
        assert_eq!(endpoint_name(&view_request.path), "getArtists");
        assert!(validate_protocol(&view_request).is_ok());
        for endpoint in ["ping", "getArtists", "stream", "getCoverArt"] {
            assert_eq!(endpoint_name(&format!("/rest/{endpoint}.view")), endpoint);
        }
        for name in ["apiKey", "t", "s"] {
            let request = request(&[("v", VERSION), (name, "value")]);
            assert!(
                validate_protocol(&request).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn parses_only_single_explicit_byte_ranges() {
        assert_eq!(
            parse_range("bytes=10-20").unwrap(),
            MediaByteRange {
                start: 10,
                end: Some(20)
            }
        );
        assert_eq!(
            parse_range("bytes=10-").unwrap(),
            MediaByteRange {
                start: 10,
                end: None
            }
        );
        assert!(parse_range("bytes=-10").is_err());
        assert!(parse_range("bytes=1-2,4-5").is_err());
        assert!(parse_range("bytes=4-2").is_err());
    }
}
