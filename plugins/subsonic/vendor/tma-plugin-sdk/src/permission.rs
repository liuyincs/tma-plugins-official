//! 插件权限声明（ABI 1.1 新增）：能力清单之外的默认拒绝边界。
//!
//! 初版 ABI 的清单只有"你是谁、接什么扩展点"；引入 WASM 宿主后，插件能发起的
//! 副作用必须显式声明、宿主逐次校验。第一项权限就是出站 HTTP：scheme + host +
//! 必填理由（`reason`）。未声明的请求在宿主侧直接拒绝（never panic，规则由
//! 宿主侧 gate 落地）。
//!
//! host 匹配语义（本 crate 不实现，规则由宿主侧 gate 落地）：
//!
//! - `example.com` 精确匹配自身，**不含子域**；
//! - `*.example.com` 匹配任意深层子域（`a.example.com`、`a.b.example.com`），
//!   不匹配裸域 `example.com`；
//! - host 不含端口 = 任意端口；含端口（`example.com:8080`）= 精确端口；
//! - host 大小写不敏感（宿主归一为小写比较）。
//!
//! ABI 1.4 新增 [`Permission::Ai`]：插件申请调用宿主的**单轮** AI 对话
//! （`ai_chat`，见 [`crate::ai_host`]）。AI 配置是服务端共享凭据、按调用计费，
//! 因此与出站 HTTP 同样默认拒绝、逐次校验；`reason` 会在安装确认界面原样展示
//! 给用户（与 Http 的 reason 同口径：必填非空）。
//!
//! ABI 1.6 新增 [`Permission::Capability`]：插件申请宿主提供的版本化业务能力，
//! 例如 `catalog.read`、`identity.read` 和 `media.stream`。能力名称与用途说明
//! 一并展示，并由各能力的宿主 DTO 再做逐次边界校验。

use serde::{Deserialize, Serialize};

/// 允许的出站 URL scheme。默认 `https`（宽松的 `http` 需显式声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpScheme {
    #[default]
    Https,
    Http,
}

/// HTTP 权限上的可选流量声明：配额组与窗口，由宿主统一执行。
///
/// 缺省省略键（旧清单零迁移）。`group` 只用于共享额度，不能扩大 HTTP 白名单。
/// 实际额度取插件声明与宿主安全上限的更严值；管理员提额不在本字段里完成。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HttpTrafficPolicy {
    /// 配额组身份（反向域名，如 `org.musicbrainz.public-api`）。
    /// 不同插件、不同 host 可声明同一组以共享令牌桶、并发上限与冷却。
    pub group: String,
    /// 窗口内允许的请求数；与 [`period_seconds`] 一起表达平均速率。
    pub requests: u32,
    /// 速率窗口（秒）。
    pub period_seconds: u32,
    /// 令牌桶容量（突发上限）。
    pub burst: u32,
    /// 同一配额组同时在途的请求上限（持有到响应体读完）。
    pub max_in_flight: u32,
    /// 视为限流并立即打开组冷却的 HTTP 状态码（如 429，或 MusicBrainz 的 503）。
    pub rate_limited_statuses: Vec<u16>,
}

/// 宿主为未声明 `traffic` 的请求生成回退组时使用的保留前缀。
pub const FALLBACK_TRAFFIC_GROUP_PREFIX: &str = "host:";

/// 单项权限。serde 形态为外标签 + snake_case 变体名：
/// `{"http":{"scheme":"https","host":...,"reason":...}}`、
/// `{"ai":{"reason":...}}`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// 出站 HTTP：仅可请求声明 scheme+host 的 URL（宿主逐次校验）。
    Http {
        /// 缺省 `https`（serde default）。
        #[serde(default)]
        scheme: HttpScheme,
        /// 目标 host；可含端口（不含端口 = 任意端口），支持 `*.domain` 子域通配。
        host: String,
        /// 人读的用途说明（必填非空：权限审计的最低要求）。
        reason: String,
        /// 可选流量声明。缺省省略；旧清单仍可加载，由宿主按目标域名回退。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        traffic: Option<HttpTrafficPolicy>,
    },
    /// 调用宿主单轮 AI 对话（ABI 1.4 起，`ai_chat`）。
    ///
    /// 只授"单轮、非连续"的对话能力（无上下文延续、无工具调用），配额与
    /// `max_tokens` 由宿主统一写死，插件不可指定。凭据是服务端共享资源，
    /// 按调用计费，故默认拒绝；`reason` 是用户在安装确认界面看到的唯一解释。
    Ai {
        /// 人读的用途说明（必填非空：与 [`Permission::Http`] 的 reason 同口径）。
        reason: String,
    },
    /// 调用宿主版本化业务能力（ABI 1.6 起）。
    ///
    /// `name` 是稳定的 capability 名称（例如 `catalog.read`、`identity.read`
    /// 或 `media.stream`），实际请求仍须经过对应宿主 DTO 的逐次校验。
    Capability { name: String, reason: String },
}

/// ABI 1.6 当前可申请的宿主业务能力。新增能力必须扩展 DTO 与宿主闸门后再加入。
pub const INBOUND_CAPABILITIES: &[&str] = &["catalog.read", "identity.read", "media.stream"];

impl Permission {
    /// 结构校验：host/reason 非空、host 不夹带 scheme 前缀或路径。
    ///
    /// host 写成 `https://foo`、`foo/bar` 这类"把 URL 当 host"的常见笔误在此拦截，
    /// 而不是等到运行时白名单全部失配。
    pub fn validate(&self) -> Result<(), PermissionError> {
        match self {
            Permission::Http {
                host,
                reason,
                traffic,
                ..
            } => {
                if host.trim().is_empty() {
                    return Err(PermissionError::EmptyHost);
                }
                if reason.trim().is_empty() {
                    return Err(PermissionError::EmptyReason);
                }
                if host.contains("://") || host.contains('/') {
                    return Err(PermissionError::HostLooksLikeUrl(host.clone()));
                }
                if let Some(traffic) = traffic {
                    validate_http_traffic(traffic)?;
                }
                Ok(())
            }
            Permission::Ai { reason } => {
                if reason.trim().is_empty() {
                    return Err(PermissionError::EmptyReason);
                }
                Ok(())
            }
            Permission::Capability { name, reason } => {
                if reason.trim().is_empty() {
                    return Err(PermissionError::EmptyReason);
                }
                if !is_valid_capability_name(name) {
                    return Err(PermissionError::InvalidCapability(name.clone()));
                }
                if !INBOUND_CAPABILITIES.contains(&name.as_str()) {
                    return Err(PermissionError::UnsupportedCapability(name.clone()));
                }
                Ok(())
            }
        }
    }
}

/// 单项权限校验错误（宿主侧经 `ManifestError::InvalidPermissionAt` 上抛）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PermissionError {
    #[error("permission host must not be empty")]
    EmptyHost,
    #[error("permission reason must not be empty")]
    EmptyReason,
    #[error("permission host must be a bare host, got url-like value: {0:?}")]
    HostLooksLikeUrl(String),
    /// `traffic` 声明非法（空组、保留前缀、零窗口、空限流状态列表等）。
    #[error("invalid http traffic policy: {0}")]
    InvalidTraffic(String),
    #[error("invalid capability name: {0:?}")]
    InvalidCapability(String),
    #[error("unsupported capability name: {0:?}")]
    UnsupportedCapability(String),
}

fn is_valid_capability_name(name: &str) -> bool {
    let mut segments = name.split('.');
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty()
        || !first.as_bytes()[0].is_ascii_lowercase()
        || !first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return false;
    }
    segments.all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn validate_http_traffic(traffic: &HttpTrafficPolicy) -> Result<(), PermissionError> {
    let group = traffic.group.trim();
    if group.is_empty() {
        return Err(PermissionError::InvalidTraffic(
            "group must not be empty".into(),
        ));
    }
    if group.starts_with(FALLBACK_TRAFFIC_GROUP_PREFIX) {
        return Err(PermissionError::InvalidTraffic(format!(
            "group {group:?} uses reserved prefix {FALLBACK_TRAFFIC_GROUP_PREFIX:?}"
        )));
    }
    if !is_valid_traffic_group(group) {
        return Err(PermissionError::InvalidTraffic(format!(
            "group {group:?} must be 1..=128 chars, start with a letter, and contain only [A-Za-z0-9._-]"
        )));
    }
    if traffic.requests == 0 {
        return Err(PermissionError::InvalidTraffic(
            "requests must be > 0".into(),
        ));
    }
    if traffic.period_seconds == 0 {
        return Err(PermissionError::InvalidTraffic(
            "period_seconds must be > 0".into(),
        ));
    }
    if traffic.burst == 0 {
        return Err(PermissionError::InvalidTraffic("burst must be > 0".into()));
    }
    if traffic.max_in_flight == 0 {
        return Err(PermissionError::InvalidTraffic(
            "max_in_flight must be > 0".into(),
        ));
    }
    if traffic.rate_limited_statuses.is_empty() {
        return Err(PermissionError::InvalidTraffic(
            "rate_limited_statuses must not be empty".into(),
        ));
    }
    for status in &traffic.rate_limited_statuses {
        if !(400..600).contains(status) {
            return Err(PermissionError::InvalidTraffic(format!(
                "rate_limited_statuses must be HTTP 4xx/5xx, got {status}"
            )));
        }
    }
    Ok(())
}

fn is_valid_traffic_group(group: &str) -> bool {
    let bytes = group.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    match bytes[0] {
        b'a'..=b'z' | b'A'..=b'Z' => {}
        _ => return false,
    }
    bytes[1..]
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_permission_serde_shape() {
        let json =
            r#"{"http":{"scheme":"https","host":"ws.audioscrobbler.com","reason":"artist info"}}"#;
        let p: Permission = serde_json::from_str(json).unwrap();
        assert_eq!(
            p,
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "ws.audioscrobbler.com".into(),
                reason: "artist info".into(),
                traffic: None,
            }
        );
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
    }

    #[test]
    fn scheme_defaults_to_https() {
        let p: Permission =
            serde_json::from_str(r#"{"http":{"host":"a.com","reason":"r"}}"#).unwrap();
        assert_eq!(
            p,
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "a.com".into(),
                reason: "r".into(),
                traffic: None,
            }
        );
    }

    #[test]
    fn http_scheme_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&HttpScheme::Http).unwrap(),
            r#""http""#
        );
        assert_eq!(
            serde_json::from_str::<HttpScheme>(r#""https""#).unwrap(),
            HttpScheme::Https
        );
    }

    /// AI 权限（ABI 1.4）：wire 形态为 `{"ai":{"reason":...}}`，变体名 snake_case。
    #[test]
    fn ai_permission_serde_shape() {
        let json = r#"{"ai":{"reason":"从自由文本抽取曲目列表"}}"#;
        let p: Permission = serde_json::from_str(json).unwrap();
        assert_eq!(
            p,
            Permission::Ai {
                reason: "从自由文本抽取曲目列表".into(),
            }
        );
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
    }

    /// AI 权限校验：reason 必填非空（与 Http 的 reason 同一错误变体与口径）。
    #[test]
    fn ai_permission_requires_non_empty_reason() {
        let empty = Permission::Ai { reason: "".into() };
        assert_eq!(empty.validate(), Err(PermissionError::EmptyReason));

        let blank = Permission::Ai {
            reason: "   ".into(),
        };
        assert_eq!(blank.validate(), Err(PermissionError::EmptyReason));

        let ok = Permission::Ai { reason: "x".into() };
        ok.validate().unwrap();
    }

    #[test]
    fn capability_permission_serde_and_validation() {
        let json = r#"{"capability":{"name":"catalog.read","reason":"浏览资料库"}}"#;
        let permission: Permission = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&permission).unwrap(), json);
        permission.validate().unwrap();

        let invalid = Permission::Capability {
            name: "catalog/read".into(),
            reason: "r".into(),
        };
        assert!(matches!(
            invalid.validate(),
            Err(PermissionError::InvalidCapability(_))
        ));

        let unknown = Permission::Capability {
            name: "catalog.write".into(),
            reason: "r".into(),
        };
        assert_eq!(
            unknown.validate(),
            Err(PermissionError::UnsupportedCapability(
                "catalog.write".into()
            ))
        );
    }

    /// ABI 1.6 capability 权限保持稳定的外标签形态，便于安装确认界面逐项展示用途。
    #[test]
    fn capability_permission_serde_shape_and_validation() {
        let json = r#"{"capability":{"name":"catalog.read","reason":"浏览曲库"}}"#;
        let p: Permission = serde_json::from_str(json).unwrap();
        assert_eq!(
            p,
            Permission::Capability {
                name: "catalog.read".into(),
                reason: "浏览曲库".into(),
            }
        );
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
        p.validate().unwrap();
    }

    #[test]
    fn capability_permission_rejects_blank_or_malformed_names() {
        for name in ["", "Catalog.read", "catalog/secret", "catalog..read"] {
            let p = Permission::Capability {
                name: name.into(),
                reason: "r".into(),
            };
            assert!(
                matches!(p.validate(), Err(PermissionError::InvalidCapability(_))),
                "capability name {name:?} should be rejected"
            );
        }

        let blank_reason = Permission::Capability {
            name: "catalog.read".into(),
            reason: " ".into(),
        };
        assert_eq!(blank_reason.validate(), Err(PermissionError::EmptyReason));
    }

    #[test]
    fn validate_rejects_empty_host_reason_and_url_like_host() {
        let empty_host = Permission::Http {
            host: "  ".into(),
            reason: "r".into(),
            scheme: HttpScheme::Https,
            traffic: None,
        };
        assert_eq!(empty_host.validate(), Err(PermissionError::EmptyHost));

        let empty_reason = Permission::Http {
            host: "a.com".into(),
            reason: "".into(),
            scheme: HttpScheme::Https,
            traffic: None,
        };
        assert_eq!(empty_reason.validate(), Err(PermissionError::EmptyReason));

        let url_like = Permission::Http {
            host: "https://a.com".into(),
            reason: "r".into(),
            scheme: HttpScheme::Https,
            traffic: None,
        };
        assert_eq!(
            url_like.validate(),
            Err(PermissionError::HostLooksLikeUrl("https://a.com".into()))
        );

        let with_path = Permission::Http {
            host: "a.com/api".into(),
            reason: "r".into(),
            scheme: HttpScheme::Http,
            traffic: None,
        };
        assert!(matches!(
            with_path.validate(),
            Err(PermissionError::HostLooksLikeUrl(_))
        ));
    }

    #[test]
    fn port_and_wildcard_hosts_are_accepted() {
        for host in ["example.com", "example.com:8080", "*.example.com"] {
            let p = Permission::Http {
                host: host.into(),
                reason: "r".into(),
                scheme: HttpScheme::Https,
                traffic: None,
            };
            p.validate().unwrap();
        }
    }

    fn sample_traffic() -> HttpTrafficPolicy {
        HttpTrafficPolicy {
            group: "org.example.api".into(),
            requests: 7,
            period_seconds: 10,
            burst: 1,
            max_in_flight: 1,
            rate_limited_statuses: vec![429, 503],
        }
    }

    /// 旧清单无 `traffic` 键：解析为 None，往返 JSON 逐字节不变。
    #[test]
    fn missing_traffic_roundtrips_without_the_key() {
        let json = r#"{"http":{"scheme":"https","host":"a.com","reason":"r"}}"#;
        let p: Permission = serde_json::from_str(json).unwrap();
        assert!(matches!(p, Permission::Http { traffic: None, .. }));
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
        p.validate().unwrap();
    }

    /// 未知 JSON 字段忽略（新旧宿主双向兼容，不构成 ABI 门槛）。
    #[test]
    fn unknown_http_fields_are_ignored() {
        let p: Permission = serde_json::from_str(
            r#"{"http":{"scheme":"https","host":"a.com","reason":"r","future":true}}"#,
        )
        .unwrap();
        assert!(matches!(p, Permission::Http { traffic: None, .. }));
    }

    #[test]
    fn traffic_roundtrips_when_present() {
        let p = Permission::Http {
            scheme: HttpScheme::Https,
            host: "musicbrainz.org".into(),
            reason: "ws".into(),
            traffic: Some(sample_traffic()),
        };
        p.validate().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""group":"org.example.api""#));
        assert!(json.contains(r#""rate_limited_statuses":[429,503]"#));
        assert_eq!(serde_json::from_str::<Permission>(&json).unwrap(), p);
    }

    #[test]
    fn traffic_validation_rejects_illegal_values() {
        let base = sample_traffic();
        let cases = [
            (
                HttpTrafficPolicy {
                    group: "  ".into(),
                    ..base.clone()
                },
                "group must not be empty",
            ),
            (
                HttpTrafficPolicy {
                    group: "host:example.com".into(),
                    ..base.clone()
                },
                "reserved prefix",
            ),
            (
                HttpTrafficPolicy {
                    group: "bad group".into(),
                    ..base.clone()
                },
                "start with a letter",
            ),
            (
                HttpTrafficPolicy {
                    requests: 0,
                    ..base.clone()
                },
                "requests must be > 0",
            ),
            (
                HttpTrafficPolicy {
                    period_seconds: 0,
                    ..base.clone()
                },
                "period_seconds must be > 0",
            ),
            (
                HttpTrafficPolicy {
                    burst: 0,
                    ..base.clone()
                },
                "burst must be > 0",
            ),
            (
                HttpTrafficPolicy {
                    max_in_flight: 0,
                    ..base.clone()
                },
                "max_in_flight must be > 0",
            ),
            (
                HttpTrafficPolicy {
                    rate_limited_statuses: vec![],
                    ..base.clone()
                },
                "rate_limited_statuses must not be empty",
            ),
            (
                HttpTrafficPolicy {
                    rate_limited_statuses: vec![200],
                    ..base
                },
                "HTTP 4xx/5xx",
            ),
        ];
        for (traffic, needle) in cases {
            let p = Permission::Http {
                scheme: HttpScheme::Https,
                host: "a.com".into(),
                reason: "r".into(),
                traffic: Some(traffic),
            };
            match p.validate() {
                Err(PermissionError::InvalidTraffic(msg)) => {
                    assert!(msg.contains(needle), "期望包含 {needle:?}，实际 {msg:?}");
                }
                other => panic!("期望 InvalidTraffic，实际 {other:?}"),
            }
        }
    }
}
