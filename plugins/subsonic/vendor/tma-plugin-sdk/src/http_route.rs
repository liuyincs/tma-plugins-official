//! 通用入站 HTTP 扩展点（ABI 1.6 起）的版本化 DTO。
//!
//! 入站请求与既有 [`crate::http_host`] 的出站请求分开建模。宿主负责监听、鉴权和
//! 响应头安全策略；插件只看到经过宿主筛选后的路径、查询参数、请求头和请求体，
//! 并返回一个受限的 HTTP 响应。DTO 是纯 JSON，适合直接作为 `tma_http` 的输入/输出。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 入站路由允许的 HTTP 方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpRouteMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpRouteMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

/// 清单中的一条入站路由声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRouteManifest {
    /// 以 `/` 开头的路由模式。末尾的 `/*` 表示捕获剩余路径。
    pub path: String,
    /// 该路由接受的方法；至少一个且不能重复。
    pub methods: Vec<HttpRouteMethod>,
}

/// 入站 HTTP 扩展点清单段（ABI 1.6 起）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpManifest {
    /// 插件导出的路由列表。宿主把请求匹配到路由后调用 `tma_http`。
    pub routes: Vec<HttpRouteManifest>,
}

/// 宿主转交插件的入站 HTTP 请求（`tma_http` 入参）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHttpRequest {
    /// 标准大写 HTTP 方法。
    pub method: String,
    /// 已匹配路由后的请求路径，保留开头 `/`。
    pub path: String,
    /// 查询参数。值为数组以保留重复参数的语义。
    #[serde(default)]
    pub query: BTreeMap<String, Vec<String>>,
    /// 宿主筛选后的请求头；header name 统一小写。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 请求体，缺省表示空 body。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
    /// 宿主完成认证后的用户身份。公开路由为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PluginHttpIdentity>,
}

/// 入站 HTTP 请求的认证身份快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHttpIdentity {
    pub user_id: String,
    pub username: String,
    #[serde(default)]
    pub is_admin: bool,
}

/// 插件返回给宿主的入站 HTTP 响应（`tma_http` 出参）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHttpResponse {
    /// HTTP 状态码，宿主只接受 100..=599。
    pub status: u16,
    /// 响应头。宿主会过滤 hop-by-hop 与安全敏感 header。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 响应体，缺省表示空 body。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b64: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_use_standard_uppercase_wire_names() {
        assert_eq!(
            serde_json::to_string(&HttpRouteMethod::Get).unwrap(),
            "\"GET\""
        );
        assert_eq!(HttpRouteMethod::Patch.as_str(), "PATCH");
    }

    #[test]
    fn request_and_response_roundtrip() {
        let request = PluginHttpRequest {
            method: "GET".into(),
            path: "/rest/ping".into(),
            query: BTreeMap::from([(String::from("v"), vec![String::from("1")])]),
            headers: BTreeMap::from([(String::from("accept"), String::from("application/json"))]),
            body_b64: None,
            identity: None,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<PluginHttpRequest>(&encoded).unwrap(),
            request
        );

        let response = PluginHttpResponse {
            status: 200,
            headers: BTreeMap::from([(
                String::from("content-type"),
                String::from("application/json"),
            )]),
            body_b64: Some("e30=".into()),
        };
        let encoded = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<PluginHttpResponse>(&encoded).unwrap(),
            response
        );
    }
}
