//! stub 代理：URL 子串匹配的罐头响应 + 请求记录。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::HttpHostResponse;

// ---------------------------------------------------------------------------
// stub 代理：URL 子串匹配的罐头响应 + 请求记录
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

/// 单条路由：URL 含 `needle` 即命中（首个命中优先）。
pub struct Route {
    pub needle: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub struct RouteBuilder;

impl RouteBuilder {
    /// 200 + JSON body 的罐头响应（等价原 `builtin_tests::support::RouteBuilder::json`）。
    pub fn json(needle: &str, body: serde_json::Value) -> Route {
        Route {
            needle: needle.to_string(),
            status: 200,
            headers: BTreeMap::new(),
            body: body.to_string(),
        }
    }
}

/// 记录型 stub 代理：未命中路由 → `network` 失败（测试将失败并带出 URL，
/// 与宿主 `ProxyHttpError::Network` 同码同文）。
pub struct StubProxy {
    routes: Vec<Route>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl StubProxy {
    pub fn new(routes: Vec<Route>) -> Arc<Self> {
        Arc::new(Self {
            routes,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// 已记录的出站请求（`pub(crate)`：插件实例的读取只能走
    /// [`crate::Driver::http_requests`]，不暴露第二观察面）。
    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// 已记录的出站 URL 列表（`requests` 的投影；读取走
    /// [`crate::Driver::http_urls`]）。
    pub(crate) fn urls(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.url).collect()
    }

    /// `http_request` 宿主函数的业务段：先记录再匹配（未命中也留痕）。
    pub(crate) fn dispatch(
        &self,
        method: String,
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<Vec<u8>>,
    ) -> HttpHostResponse {
        self.requests.lock().unwrap().push(RecordedRequest {
            method: method.clone(),
            url: url.clone(),
            headers,
            body,
        });
        let Some(route) = self.routes.iter().find(|r| url.contains(&r.needle)) else {
            // 宿主侧 `ProxyHttpError::Network(msg)` → code "network"、
            // error 为 Display 文本 "network: {msg}"，逐字对齐。
            return HttpHostResponse::failure(
                "network",
                format!("network: stub 代理无预设响应: {method} {url}"),
            );
        };
        HttpHostResponse::success(
            route.status,
            route.headers.clone(),
            BASE64.encode(route.body.as_bytes()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未命中路由：`code:"network"` + 错误文案与宿主 `ProxyHttpError::Network`
    /// Display 同文，且请求仍被记录（断言行可见未命中 URL）。
    #[test]
    fn stub_proxy_miss_records_and_reports_network() {
        let proxy = StubProxy::new(vec![]);
        let resp = proxy.dispatch(
            "GET".into(),
            "https://example.com/x".into(),
            BTreeMap::new(),
            None,
        );
        assert!(!resp.ok);
        assert_eq!(resp.code.as_deref(), Some("network"));
        assert_eq!(
            resp.error.as_deref(),
            Some("network: stub 代理无预设响应: GET https://example.com/x")
        );
        assert_eq!(proxy.urls(), ["https://example.com/x"]);
    }
}
