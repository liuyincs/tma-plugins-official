//! capability 桩：ABI 1.6 三条宿主函数的注入面（路由表 + 单值槽 + 请求记录）。

use std::sync::{Arc, Mutex};

use crate::{
    CAPABILITY_DTO_VERSION, CapabilityError, CatalogItem, CatalogReadRequest, CatalogReadResponse,
    IdentityReadRequest, IdentityReadResponse, MediaByteRange, MediaStreamRequest,
    MediaStreamResponse,
};

// ---------------------------------------------------------------------------
// capability 桩：ABI 1.6 三条宿主函数的注入面（路由表 + 单值槽 + 请求记录）
// ---------------------------------------------------------------------------

/// `tma_catalog_read` 路由的匹配条件：`Some` 字段要求请求同名字段相等
/// （请求的 `Option` 字段即要求为 `Some(该值)`），`None` 不约束。
/// 注意匹配无法表达「请求字段必须为 None」——不设约束的字段对 None 与
/// 任意 Some 一律放行；要断言请求确实没带某字段，用 Driver 的
/// `catalog_requests` 观察方法读出的请求 DTO 判。
///
/// `Deserialize` 只服务 cfg(test) 的契约夹具（含 `deny_unknown_fields`——
/// 路由表里写错的 matcher 字段在解析期大声失败，不静默吞成「无约束」），
/// 不进公开 API 面。
#[derive(Debug, Clone, Default)]
#[cfg_attr(test, derive(serde::Deserialize), serde(deny_unknown_fields))]
pub struct CatalogMatch {
    pub kind: Option<String>,
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl CatalogMatch {
    /// 只约束 `kind`（列表/详情查询最常用的区分维度）。
    pub fn kind(kind: &str) -> Self {
        Self {
            kind: Some(kind.into()),
            ..Default::default()
        }
    }

    /// 追加 `id` 等值约束（get-by-id 形态）。
    pub fn and_id(mut self, id: &str) -> Self {
        self.id = Some(id.into());
        self
    }

    /// 追加 `parent_id` 等值约束（children 列表形态）。
    pub fn and_parent_id(mut self, parent_id: &str) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// 追加 `query` 等值约束。
    pub fn and_query(mut self, query: &str) -> Self {
        self.query = Some(query.into());
        self
    }

    /// 追加 `cursor` 等值约束。
    pub fn and_cursor(mut self, cursor: &str) -> Self {
        self.cursor = Some(cursor.into());
        self
    }

    /// 追加 `limit` 等值约束。
    pub fn and_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub(crate) fn matches(&self, req: &CatalogReadRequest) -> bool {
        fn opt(want: &Option<String>, got: &Option<String>) -> bool {
            want.as_ref().is_none_or(|want| Some(want) == got.as_ref())
        }
        opt(&self.kind, &req.kind)
            && opt(&self.id, &req.id)
            && opt(&self.parent_id, &req.parent_id)
            && opt(&self.query, &req.query)
            && opt(&self.cursor, &req.cursor)
            && self.limit.is_none_or(|limit| limit == req.limit)
    }
}

/// `tma_media_stream` 路由的匹配条件（口径同 [`CatalogMatch`]，`Deserialize`
/// 同样只服务 cfg(test) 的契约夹具）。
#[derive(Debug, Clone, Default)]
#[cfg_attr(test, derive(serde::Deserialize), serde(deny_unknown_fields))]
pub struct MediaMatch {
    pub media_id: Option<String>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub range: Option<MediaByteRange>,
}

impl MediaMatch {
    /// 只约束 `media_id`（最常用的区分维度）。
    pub fn media_id(media_id: &str) -> Self {
        Self {
            media_id: Some(media_id.into()),
            ..Default::default()
        }
    }

    /// 追加 `codec` 等值约束。
    pub fn and_codec(mut self, codec: &str) -> Self {
        self.codec = Some(codec.into());
        self
    }

    /// 追加 `bitrate_kbps` 等值约束。
    pub fn and_bitrate_kbps(mut self, bitrate_kbps: u32) -> Self {
        self.bitrate_kbps = Some(bitrate_kbps);
        self
    }

    /// 追加 `range` 等值约束（要求请求带 `Some(range)`）。
    pub fn and_range(mut self, start: u64, end: Option<u64>) -> Self {
        self.range = Some(MediaByteRange { start, end });
        self
    }

    pub(crate) fn matches(&self, req: &MediaStreamRequest) -> bool {
        self.media_id.as_deref().is_none_or(|id| id == req.media_id)
            && self
                .codec
                .as_deref()
                .is_none_or(|c| req.codec.as_deref() == Some(c))
            && self
                .bitrate_kbps
                .is_none_or(|b| req.bitrate_kbps == Some(b))
            && self.range.is_none_or(|r| req.range == Some(r))
    }
}

/// catalog 成功响应（`version` 自动填 [`CAPABILITY_DTO_VERSION`]，无 next_cursor）。
pub fn catalog_ok(items: Vec<CatalogItem>) -> CatalogReadResponse {
    CatalogReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        items,
        next_cursor: None,
        error: None,
    }
}

/// catalog 业务失败（`ok:false` DTO——与「桩未命中」的宿主级 `Err`
/// 是两种失败形态，测试别混用）。
pub fn catalog_err(code: &str, message: &str) -> CatalogReadResponse {
    CatalogReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        items: Vec::new(),
        next_cursor: None,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// `CatalogItem` 便捷构造：必备三字段 + 其余全 `None`，测试按需覆盖字段。
pub fn catalog_item(id: &str, kind: &str, title: &str) -> CatalogItem {
    CatalogItem {
        id: id.into(),
        kind: kind.into(),
        title: title.into(),
        parent_id: None,
        album_id: None,
        duration_ms: None,
        artist: None,
        album: None,
        track_number: None,
        disc_number: None,
        year: None,
        genre: None,
        format: None,
        bitrate: None,
        size: None,
        cover_art_id: None,
    }
}

/// 已认证身份快照（`is_admin` 按宿主语义注入）。
pub fn identity_ok(user_id: &str, username: &str, is_admin: bool) -> IdentityReadResponse {
    IdentityReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        user_id: Some(user_id.into()),
        username: Some(username.into()),
        is_admin,
        error: None,
    }
}

/// 未认证/身份读取业务失败（`ok:false` DTO）。
pub fn identity_err(code: &str, message: &str) -> IdentityReadResponse {
    IdentityReadResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        user_id: None,
        username: None,
        is_admin: false,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// media 成功响应最小构造；`content_length`/`content_range`/`stream_id`
/// 需要时在返回结构上直接改字段。
pub fn media_ok(status: u16, content_type: &str, body_b64: Option<String>) -> MediaStreamResponse {
    MediaStreamResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: true,
        status,
        content_type: Some(content_type.into()),
        content_length: None,
        content_range: None,
        body_b64,
        stream_id: None,
        error: None,
    }
}

/// media 业务失败（`status` 如 404/416 由插件自行翻译成协议错误）。
pub fn media_err(status: u16, code: &str, message: &str) -> MediaStreamResponse {
    MediaStreamResponse {
        version: CAPABILITY_DTO_VERSION,
        ok: false,
        status,
        content_type: None,
        content_length: None,
        content_range: None,
        body_b64: None,
        stream_id: None,
        error: Some(CapabilityError::new(code, message)),
    }
}

/// 三条 capability 宿主函数的注入面：catalog/media 各一张「匹配条件 → 响应」
/// 路由表（先注册先命中），identity 单值槽；进入派发的每次调用都先记录
/// 完整请求 DTO（在宿主函数入口校验/权限门阶段被拒绝的调用不进入桩派发、
/// 不留痕——与宿主把校验放在 provider 之前一致）。
///
/// 未配置（identity 槽空 / 路由表为空）或未命中的调用返回宿主函数级 `Err`
/// （整个导出调用当场失败，错误消息带 capability 名与请求摘要）——与
/// [`StubProxy`](crate::StubProxy) 「先记录再明确失败」同精神，但刻意不用
/// `ok:false` DTO，避免把桩缺配置误读成已配置的业务失败。
#[derive(Default)]
pub struct CapabilityStub {
    identity: Mutex<Option<IdentityReadResponse>>,
    catalog: Mutex<Vec<(CatalogMatch, CatalogReadResponse)>>,
    media: Mutex<Vec<(MediaMatch, MediaStreamResponse)>>,
    identity_requests: Mutex<Vec<IdentityReadRequest>>,
    catalog_requests: Mutex<Vec<CatalogReadRequest>>,
    media_requests: Mutex<Vec<MediaStreamRequest>>,
}

impl CapabilityStub {
    /// 全未配置的空桩：进入桩派发的任何调用都先记录后 `Err`（入口校验或
    /// 权限门拒绝的调用不进入派发——宿主函数直接写回结构化错误 DTO）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 已认证身份 + 空路由表（入站 HTTP 插件的最常用入口）。
    pub fn authenticated(user_id: &str, username: &str, is_admin: bool) -> Arc<Self> {
        Self::new().with_identity(identity_ok(user_id, username, is_admin))
    }

    /// 写入 identity 槽位（`ok:false` 等形态也走这里）；返回同一 Arc 供链式。
    pub fn with_identity(self: Arc<Self>, response: IdentityReadResponse) -> Arc<Self> {
        *self.identity.lock().unwrap() = Some(response);
        self
    }

    /// 追加一条 catalog 路由（先注册先命中）；返回同一 Arc 供链式。
    pub fn with_catalog(
        self: Arc<Self>,
        matcher: CatalogMatch,
        response: CatalogReadResponse,
    ) -> Arc<Self> {
        self.catalog.lock().unwrap().push((matcher, response));
        self
    }

    /// 追加一条 media 路由（先注册先命中）；返回同一 Arc 供链式。
    pub fn with_media(
        self: Arc<Self>,
        matcher: MediaMatch,
        response: MediaStreamResponse,
    ) -> Arc<Self> {
        self.media.lock().unwrap().push((matcher, response));
        self
    }

    /// 已记录的 `tma_catalog_read` 入参（含未命中调用）。插件实例的读取
    /// 只能走 [`crate::Driver`] 的同名观察方法，这里收 `pub(crate)`。
    pub(crate) fn catalog_requests(&self) -> Vec<CatalogReadRequest> {
        self.catalog_requests.lock().unwrap().clone()
    }

    /// 已记录的 `tma_identity_read` 入参（含未配置调用）；读取走
    /// [`crate::Driver`]。
    pub(crate) fn identity_requests(&self) -> Vec<IdentityReadRequest> {
        self.identity_requests.lock().unwrap().clone()
    }

    /// 已记录的 `tma_media_stream` 入参（含未命中调用）；读取走
    /// [`crate::Driver`]。
    pub(crate) fn media_requests(&self) -> Vec<MediaStreamRequest> {
        self.media_requests.lock().unwrap().clone()
    }

    /// `tma_catalog_read` 宿主函数的业务段：先记录再按路由表匹配。
    pub(crate) fn dispatch_catalog(
        &self,
        req: CatalogReadRequest,
    ) -> Result<CatalogReadResponse, extism::Error> {
        self.catalog_requests.lock().unwrap().push(req.clone());
        self.catalog
            .lock()
            .unwrap()
            .iter()
            .find(|(matcher, _)| matcher.matches(&req))
            .map(|(_, response)| response.clone())
            .ok_or_else(|| extism::Error::msg(format!("tma_catalog_read 桩无预设响应: {req:?}")))
    }

    /// `tma_identity_read` 宿主函数的业务段：先记录再取单值槽。
    pub(crate) fn dispatch_identity(
        &self,
        req: IdentityReadRequest,
    ) -> Result<IdentityReadResponse, extism::Error> {
        self.identity_requests.lock().unwrap().push(req.clone());
        self.identity
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| extism::Error::msg(format!("tma_identity_read 桩无预设响应: {req:?}")))
    }

    /// `tma_media_stream` 宿主函数的业务段：先记录再按路由表匹配。
    pub(crate) fn dispatch_media(
        &self,
        req: MediaStreamRequest,
    ) -> Result<MediaStreamResponse, extism::Error> {
        self.media_requests.lock().unwrap().push(req.clone());
        self.media
            .lock()
            .unwrap()
            .iter()
            .find(|(matcher, _)| matcher.matches(&req))
            .map(|(_, response)| response.clone())
            .ok_or_else(|| extism::Error::msg(format!("tma_media_stream 桩无预设响应: {req:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// capability 桩命中：返回注入响应并把完整请求 DTO 留痕。
    #[test]
    fn capability_stub_hit_responds_and_records() {
        let stub = CapabilityStub::authenticated("u-1", "alice", true).with_catalog(
            CatalogMatch::kind("artist"),
            catalog_ok(vec![catalog_item("a-1", "artist", "Artist A")]),
        );

        let resp = stub
            .dispatch_catalog(CatalogReadRequest {
                version: CAPABILITY_DTO_VERSION,
                kind: Some("artist".into()),
                limit: 20,
                ..Default::default()
            })
            .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.items[0].id, "a-1");

        let recorded = stub.catalog_requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].kind.as_deref(), Some("artist"));
        assert_eq!(recorded[0].limit, 20);

        let identity = stub
            .dispatch_identity(IdentityReadRequest {
                version: CAPABILITY_DTO_VERSION,
            })
            .unwrap();
        assert!(identity.ok && identity.is_admin);
        assert_eq!(identity.username.as_deref(), Some("alice"));
        assert_eq!(stub.identity_requests().len(), 1);
    }

    /// capability 桩未配置/未命中：宿主函数级 `Err`（消息带 capability 名与
    /// 请求摘要），但请求仍先留痕——与 StubProxy 未命中同精神。
    #[test]
    fn capability_stub_miss_errors_and_still_records() {
        let stub = CapabilityStub::new();

        let err = stub
            .dispatch_catalog(CatalogReadRequest {
                kind: Some("song".into()),
                id: Some("s-1".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("tma_catalog_read"), "{err}");
        assert!(err.to_string().contains("s-1"), "{err}");
        assert_eq!(stub.catalog_requests().len(), 1);

        let err = stub
            .dispatch_identity(IdentityReadRequest { version: 1 })
            .unwrap_err();
        assert!(err.to_string().contains("tma_identity_read"), "{err}");
        assert_eq!(stub.identity_requests().len(), 1);

        let err = stub
            .dispatch_media(MediaStreamRequest {
                version: 1,
                media_id: "m-1".into(),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("tma_media_stream"), "{err}");
        assert_eq!(stub.media_requests().len(), 1);

        // 路由表非空但字段不匹配同样走宿主级 Err。
        let stub =
            CapabilityStub::new().with_catalog(CatalogMatch::kind("artist"), catalog_ok(vec![]));
        assert!(
            stub.dispatch_catalog(CatalogReadRequest {
                kind: Some("album".into()),
                ..Default::default()
            })
            .is_err()
        );
    }

    /// 字段匹配是「全部约束同时满足」：多约束路由不被部分命中。
    #[test]
    fn capability_stub_match_requires_all_set_fields() {
        let stub = CapabilityStub::new().with_media(
            MediaMatch::media_id("m-1").and_codec("flac"),
            media_ok(200, "audio/flac", None),
        );
        // codec 不命中 → Err。
        assert!(
            stub.dispatch_media(MediaStreamRequest {
                media_id: "m-1".into(),
                codec: Some("mp3".into()),
                ..Default::default()
            })
            .is_err()
        );
        // 全约束命中 → ok。
        let resp = stub
            .dispatch_media(MediaStreamRequest {
                media_id: "m-1".into(),
                codec: Some("flac".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(resp.ok);
        assert_eq!(stub.media_requests().len(), 2, "未命中也留痕");
    }
}
