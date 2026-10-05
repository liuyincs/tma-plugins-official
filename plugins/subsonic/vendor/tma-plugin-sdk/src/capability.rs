//! ABI 1.6 capability host functions.
//!
//! These DTOs deliberately contain only stable JSON data.  The host owns the
//! storage and authentication context; a plugin can request data through the
//! `tma_*` host functions without receiving a database handle or an internal
//! HTTP client.

use serde::{Deserialize, Serialize};

/// Version of the capability DTO wire format.
pub const CAPABILITY_DTO_VERSION: u16 = 1;

/// A stable error returned by a capability host function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityError {
    pub code: String,
    pub message: String,
}

impl CapabilityError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A compact catalog item.  Additional fields can be added in a later DTO
/// version without changing the meaning of existing fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art_id: Option<String>,
}

/// Read-only catalog query.  `cursor` is opaque to the plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogReadRequest {
    #[serde(default = "default_version")]
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default = "default_catalog_limit")]
    pub limit: u32,
}

/// Read-only catalog result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogReadResponse {
    #[serde(default = "default_version")]
    pub version: u16,
    pub ok: bool,
    #[serde(default)]
    pub items: Vec<CatalogItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CapabilityError>,
}

/// Request the identity associated with the current plugin invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityReadRequest {
    #[serde(default = "default_version")]
    pub version: u16,
}

/// A stable identity snapshot.  Passwords, tokens and external credentials
/// are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityReadResponse {
    #[serde(default = "default_version")]
    pub version: u16,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CapabilityError>,
}

/// An inclusive byte range for media streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaByteRange {
    pub start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

/// Capabilities advertised by the protocol client for one media request.
///
/// The host treats this as a hint only: the selected format is still checked
/// against the server's format registry and the caller's requested codec.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClientCapabilities {
    #[serde(default)]
    pub supports_range: bool,
    #[serde(default)]
    pub supports_transcoding: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bitrate_kbps: Option<u32>,
}

/// Read a media stream by stable media id.  The host owns range validation and
/// may return 200, 206, 404 or 416 through the response DTO.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaStreamRequest {
    #[serde(default = "default_version")]
    pub version: u16,
    pub media_id: String,
    /// Output codec id. `None` means the original file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// Requested output bitrate in kbps, when the codec supports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<MediaByteRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<MediaClientCapabilities>,
}

/// A bounded media response.  `body_b64` is optional to support future host
/// streaming while preserving a simple ABI 1.6 implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaStreamResponse {
    #[serde(default = "default_version")]
    pub version: u16,
    pub ok: bool,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
    /// Opaque host-owned stream handle. The HTTP adapter can use it to serve
    /// a large file without copying the whole media object through WASM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CapabilityError>,
}

fn default_version() -> u16 {
    CAPABILITY_DTO_VERSION
}

fn default_catalog_limit() -> u32 {
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_default_to_wire_version_one() {
        let request: CatalogReadRequest = serde_json::from_str(r#"{"query":"Bowie"}"#).unwrap();
        assert_eq!(request.version, CAPABILITY_DTO_VERSION);
        assert_eq!(request.limit, 100);

        let identity: IdentityReadRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(identity.version, CAPABILITY_DTO_VERSION);
    }

    #[test]
    fn catalog_response_has_stable_error_shape() {
        let response = CatalogReadResponse {
            version: CAPABILITY_DTO_VERSION,
            ok: false,
            items: vec![],
            next_cursor: None,
            error: Some(CapabilityError::new("forbidden", "permission denied")),
        };
        let value: serde_json::Value = serde_json::to_value(response).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["error"]["code"], "forbidden");
    }

    #[test]
    fn media_range_roundtrips_and_keeps_open_end() {
        let request = MediaStreamRequest {
            version: CAPABILITY_DTO_VERSION,
            media_id: "track-1".into(),
            codec: Some("raw".into()),
            bitrate_kbps: None,
            range: Some(MediaByteRange {
                start: 100,
                end: None,
            }),
            client_capabilities: Some(MediaClientCapabilities {
                supports_range: true,
                ..Default::default()
            }),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains("end"));
        assert_eq!(
            serde_json::from_str::<MediaStreamRequest>(&encoded).unwrap(),
            request
        );
    }

    #[test]
    fn media_stream_handle_is_optional_and_roundtrips() {
        let response = MediaStreamResponse {
            version: CAPABILITY_DTO_VERSION,
            ok: true,
            status: 200,
            content_type: Some("audio/flac".into()),
            content_length: Some(20_000_000),
            content_range: None,
            body_b64: None,
            stream_id: Some("track-1".into()),
            error: None,
        };
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(encoded.contains("stream_id"));
        assert_eq!(
            serde_json::from_str::<MediaStreamResponse>(&encoded).unwrap(),
            response
        );
    }
}
