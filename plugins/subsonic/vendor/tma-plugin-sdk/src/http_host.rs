//! 宿主 `http_request` 函数跨边界 JSON 协议（ABI 1.1+）。
//!
//! 字段名与线格式稳定：guest 经 extism host fn 传 offset，载荷为 UTF-8 JSON。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::message::RateLimitRetry;

/// 插件→宿主 HTTP 请求（`http_request` 入参）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHostRequest {
    #[serde(default = "default_method")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}

/// 宿主→插件 HTTP 响应（`http_request` 出参）。
///
/// 成功：`ok: true` + `status` / `headers` / `body_b64`；
/// 失败：`ok: false` + `error` / `code`（其余字段可缺省）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHostResponse {
    pub ok: bool,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// 宿主建议的退避秒数（来源冷却等本地拒绝场景，`code = "rate_limited"` 时携带）。
    /// 增补字段：缺省省略键，新旧 guest/宿主双向兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// 限流所属配额组。缺省省略键，新旧 guest/宿主双向兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_scope: Option<String>,
}

impl HttpHostResponse {
    /// 构造成功响应（`body_b64` 由调用方编码）。
    pub fn success(
        status: u16,
        headers: BTreeMap<String, String>,
        body_b64: impl Into<String>,
    ) -> Self {
        Self {
            ok: true,
            status,
            headers,
            body_b64: Some(body_b64.into()),
            error: None,
            code: None,
            retry_after_secs: None,
            retry_scope: None,
        }
    }

    /// 构造失败响应。
    pub fn failure(code: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            status: 0,
            headers: BTreeMap::new(),
            body_b64: None,
            error: Some(error.into()),
            code: Some(code.into()),
            retry_after_secs: None,
            retry_scope: None,
        }
    }

    /// 构造携带退避元数据的失败响应（宿主来源冷却的本地拒绝）。
    pub fn failure_with_retry(
        code: impl Into<String>,
        error: impl Into<String>,
        retry: RateLimitRetry,
    ) -> Self {
        Self {
            retry_after_secs: retry.retry_after_secs,
            retry_scope: retry.retry_scope,
            ..Self::failure(code, error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_shape() {
        let req = HttpHostRequest {
            method: "GET".into(),
            url: "https://example.com/x".into(),
            headers: BTreeMap::new(),
            body_b64: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"method":"GET","url":"https://example.com/x","headers":{}}"#
        );
        assert_eq!(serde_json::from_str::<HttpHostRequest>(&json).unwrap(), req);
    }

    #[test]
    fn response_ok_and_err_shapes() {
        let ok = HttpHostResponse::success(200, BTreeMap::new(), "aGk=");
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains(r#""ok":true"#));
        assert!(json.contains(r#""body_b64":"aGk=""#));

        let err = HttpHostResponse::failure("forbidden", "denied");
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"ok":false,"status":0,"headers":{},"error":"denied","code":"forbidden"}"#
        );
    }
}
