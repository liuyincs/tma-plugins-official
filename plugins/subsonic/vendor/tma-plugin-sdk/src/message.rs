//! 宿主↔插件通信消息：信封式请求/响应 + 纯数据错误。
//!
//! 初版 ABI 只定义形状、不接运行时：未来 WASM 沙箱里这两端就是跨边界字节流的
//! 类型化视图。信封 `id` 用于请求/响应关联（一次调用一往返，无流式）。

use serde::{Deserialize, Serialize};

use crate::scrape::{EntityQuery, ScrapeResult, ScrapedOfficialAlbum};

/// 限流退避元数据（wire JSON 仍扁平暴露在 [`PluginError`] / [`HttpHostResponse`]）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RateLimitRetry {
    pub retry_after_secs: Option<u64>,
    pub retry_scope: Option<String>,
}

impl From<Option<u64>> for RateLimitRetry {
    fn from(retry_after_secs: Option<u64>) -> Self {
        Self {
            retry_after_secs,
            retry_scope: None,
        }
    }
}

/// 宿主→插件请求信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRequest {
    /// 关联 id：响应必须原样带回。
    pub id: u64,
    /// 请求操作。
    pub op: PluginOp,
}

/// 请求操作。serde 形态为外标签 + snake_case 变体名
/// （如 `{"scrape":{"query":{...}}}`），guest 侧按标签分发。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOp {
    /// 单实体刮削（对应 `ScrapeProvider::fetch`）。
    Scrape { query: EntityQuery },
    /// 艺术家官方碟谱 browse（对应 `ScrapeProvider::browse_artist_discography`）。
    BrowseDiscography { artist_mbid: String },
}

/// 插件→宿主响应信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginResponse {
    /// 关联 id：与请求一一对应。
    pub id: u64,
    /// 响应载荷。
    pub outcome: PluginOutcome,
}

/// 响应载荷：成功按操作回包，失败给纯数据错误。
///
/// `Scrape` 载荷用 [`Box`]间接持有：`ScrapeResult` 远大于其余变体，装箱
/// 缩小信封本体；serde 对 Box 透明，线上 JSON 形态不受影响。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOutcome {
    /// `scrape` 操作的成功回包。
    Scrape(Box<ScrapeResult>),
    /// `browse_discography` 操作的成功回包。
    Discography(Vec<ScrapedOfficialAlbum>),
    /// 失败回包（请求与操作两种失败共用）。
    Error(PluginError),
}

/// `tma_action` 可选导出的请求（独立于 [`PluginOp`] scrape 管线，ABI 1.2 起）。
///
/// 形态（服务端 ↔ 插件直接 JSON in/out，无关联 id：instance-per-call 单飞行）：
/// `{"action_id": string, "payload": object|null, "locale": string|null}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginActionRequest {
    /// 目标动作 id（manifest `actions[].id`）。
    pub action_id: String,
    /// 调用方附带载荷（任意 object；null/缺省 = 无）。
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    /// 调用方界面 locale（如 `zh`/`en`；插件据此本地化返回 message，null = 未指定）。
    #[serde(default)]
    pub locale: Option<String>,
}

/// `tma_action` 可选导出的响应：`{"ok": bool, "message": string, "details": string|null}`。
///
/// `ok:false` 即业务失败（动作协议没有独立错误信封，区别于 scrape 的
/// [`PluginOutcome::Error`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginActionResponse {
    pub ok: bool,
    /// 人类可读结果文案（调用方界面直接展示）。
    pub message: String,
    /// 补充细节（长文本/诊断信息；null = 无）。
    #[serde(default)]
    pub details: Option<String>,
    /// 宿主落盘密钥（ABI 1.3 起，可选）：插件请求宿主把若干键值合并进
    /// **发起用户**的插件配置密文存储（`plugin_user_configs`）。
    ///
    /// 典型场景是授权换票或事件处理中途发现令牌失效：插件用配置里的凭据
    /// 向上游换取长期令牌（如 Last.fm session_key），经此字段交宿主加密落盘，
    /// 下次实例化经 runtime_config 注入——插件全程无状态。动作仅 `ok:true`
    /// 时落盘；事件不论成败均可写回。键必须在 manifest `user_config_secrets`
    /// 白名单内，白名单外的键由宿主拒绝。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_secrets: Option<std::collections::BTreeMap<String, String>>,
    /// 宿主转交前端打开的上游授权页（Last.fm 网页授权等）。缺省省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    /// 宿主后续调用 `complete` 时原样带回（如 Last.fm `auth.getToken`）。
    /// 不下发前端、不落盘、不进 details。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
    /// 授权尚未完成（用户还在上游页面）；宿主应继续调用 `complete`。
    #[serde(default, skip_serializing_if = "skip_if_false")]
    pub pending: bool,
}

fn skip_if_false(value: &bool) -> bool {
    !*value
}

impl PluginActionResponse {
    /// 成功响应（无细节）。
    pub fn success(message: String) -> Self {
        Self {
            ok: true,
            message,
            details: None,
            persist_secrets: None,
            authorization_url: None,
            resume_token: None,
            pending: false,
        }
    }

    /// 失败响应（无细节）。
    pub fn failure(message: String) -> Self {
        Self {
            ok: false,
            message,
            details: None,
            persist_secrets: None,
            authorization_url: None,
            resume_token: None,
            pending: false,
        }
    }

    /// 网页授权进行中：打开 `authorization_url`，`resume_token` 供 complete 原样带回。
    pub fn pending_authorization(
        message: String,
        authorization_url: String,
        resume_token: String,
    ) -> Self {
        Self {
            ok: true,
            message,
            details: None,
            persist_secrets: None,
            authorization_url: Some(authorization_url),
            resume_token: Some(resume_token),
            pending: true,
        }
    }

    /// 仍在等待用户于上游页面点允许（轮询 complete 的中间态）。
    pub fn pending_wait(message: String) -> Self {
        Self {
            ok: true,
            message,
            details: None,
            persist_secrets: None,
            authorization_url: None,
            resume_token: None,
            pending: true,
        }
    }
}

impl Default for PluginActionResponse {
    fn default() -> Self {
        Self::failure(String::new())
    }
}

/// 跨边界纯数据错误：`code` 稳定可编程判别，`message` 供人读。
///
/// 进程内错误（reqwest/sqlx 等）不允许穿透边界；宿主/guest 各自负责把本端
/// 错误投影成该形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: PluginErrorCode,
    pub message: String,
    /// 宿主据此决定重试退避。
    pub retryable: bool,
    /// 上游建议的退避秒数（429/503 的 Retry-After、来源限流窗口等）。
    ///
    /// 增补字段：`None` 时线形态省略键，旧插件 JSON 与新插件 JSON 宿主都能解析；
    /// 新旧宿主对未知键也一律忽略，双向兼容，不构成 ABI 门槛变更。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// 限流所属配额组（manifest `traffic.group` 或宿主按域名生成的回退组）。
    ///
    /// 缺省省略键：旧插件 JSON 与新插件 JSON 宿主都能解析，不构成 ABI 门槛。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_scope: Option<String>,
}

impl PluginError {
    /// 按 `code` 自动填充与 code 一致的 `retryable`（Network 除外需显式指定）。
    pub fn new(code: PluginErrorCode, message: String) -> Self {
        let retryable = match code {
            PluginErrorCode::RateLimited => true,
            PluginErrorCode::PermanentFailure => false,
            PluginErrorCode::Network => false,
            PluginErrorCode::InvalidArgument => false,
            PluginErrorCode::Unsupported => false,
            PluginErrorCode::Internal => false,
        };
        Self {
            code,
            message,
            retryable,
            retry_after_secs: None,
            retry_scope: None,
        }
    }

    /// Network 错误：`retryable` 由调用方按超时/连接等语义指定。
    pub fn network(message: String, retryable: bool) -> Self {
        Self {
            code: PluginErrorCode::Network,
            message,
            retryable,
            retry_after_secs: None,
            retry_scope: None,
        }
    }

    /// 限流错误：一次设齐退避秒数与配额组。
    pub fn rate_limited(message: String, retry: impl Into<RateLimitRetry>) -> Self {
        let retry = retry.into();
        Self {
            code: PluginErrorCode::RateLimited,
            message,
            retryable: true,
            retry_after_secs: retry.retry_after_secs,
            retry_scope: retry.retry_scope,
        }
    }

    /// 附带退避元数据（构建器风格：消耗自身返回）。
    pub fn with_retry(mut self, retry: RateLimitRetry) -> Self {
        self.retry_after_secs = retry.retry_after_secs;
        self.retry_scope = retry.retry_scope;
        self
    }

    /// 附带退避秒数（构建器风格：消耗自身返回）。
    pub fn with_retry_after_secs(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// 附带配额组身份（构建器风格：消耗自身返回）。
    pub fn with_retry_scope(mut self, scope: impl Into<String>) -> Self {
        self.retry_scope = Some(scope.into());
        self
    }
}

/// 错误码闭集：新增变体属 minor ABI 变更。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorCode {
    /// 网络不可达 / 连接失败 / 上游 5xx。
    Network,
    /// 上游限流（429 / MusicBrainz 503）。
    RateLimited,
    /// 请求参数不合法。
    InvalidArgument,
    /// 插件不支持该操作。
    Unsupported,
    /// 上游永久拒绝（4xx 非 429 等），重试无意义。
    PermanentFailure,
    /// 插件内部错误（解析失败、bug）。
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrape::{FetchedId, ScrapeEntityKind, ScrapeResult};

    #[test]
    fn request_envelope_serde_shape() {
        let query = EntityQuery {
            kind: ScrapeEntityKind::Artist,
            mbid: Some("mbid-1".into()),
            isrc: None,
            name: Some("艺人".into()),
            artist_name: None,
            known_external_ids: vec![FetchedId {
                provider: "wikidata".into(),
                external_id: "Q1".into(),
                url: None,
            }],
            library_types: Vec::new(),
        };
        let req = PluginRequest {
            id: 7,
            op: PluginOp::Scrape { query },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.starts_with(r#"{"id":7,"op":{"scrape":{"query":{"#));
        assert_eq!(serde_json::from_str::<PluginRequest>(&json).unwrap(), req);

        let browse = PluginRequest {
            id: 8,
            op: PluginOp::BrowseDiscography {
                artist_mbid: "mbid-2".into(),
            },
        };
        let json = serde_json::to_string(&browse).unwrap();
        assert_eq!(
            json,
            r#"{"id":8,"op":{"browse_discography":{"artist_mbid":"mbid-2"}}}"#
        );
    }

    #[test]
    fn response_envelope_serde_shape() {
        let resp = PluginResponse {
            id: 7,
            outcome: PluginOutcome::Scrape(Box::default()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            serde_json::from_str::<PluginResponse>(&json).unwrap(),
            resp,
            "空 ScrapeResult 必须能原样往返"
        );

        let discography = PluginResponse {
            id: 8,
            outcome: PluginOutcome::Discography(Vec::new()),
        };
        let json = serde_json::to_string(&discography).unwrap();
        assert_eq!(json, r#"{"id":8,"outcome":{"discography":[]}}"#);
    }

    #[test]
    fn error_outcome_is_pure_data() {
        let err = PluginError::new(PluginErrorCode::RateLimited, "upstream 429".into());
        assert!(err.retryable);
        let resp = PluginResponse {
            id: 9,
            outcome: PluginOutcome::Error(err),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"id":9,"outcome":{"error":{"code":"rate_limited","message":"upstream 429","retryable":true}}}"#
        );
        assert_eq!(serde_json::from_str::<PluginResponse>(&json).unwrap(), resp);
    }

    /// `tma_action` 信封：线形态与规格逐字一致（payload/locale 可 null/缺省）。
    #[test]
    fn action_envelope_serde_shape() {
        let req = PluginActionRequest {
            action_id: "refresh_cache".into(),
            payload: Some(serde_json::json!({"force": true})),
            locale: Some("zh".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"action_id":"refresh_cache","payload":{"force":true},"locale":"zh"}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginActionRequest>(&json).unwrap(),
            req
        );

        // 最小调用形态：payload/locale 缺省解析为 None（线形态 null/缺省等价）。
        let minimal: PluginActionRequest = serde_json::from_str(r#"{"action_id":"ping"}"#).unwrap();
        assert_eq!(minimal.payload, None);
        assert_eq!(minimal.locale, None);
        let nullified: PluginActionRequest =
            serde_json::from_str(r#"{"action_id":"ping","payload":null,"locale":null}"#).unwrap();
        assert_eq!(nullified.payload, None);
        assert_eq!(nullified.locale, None);

        let resp = PluginActionResponse {
            ok: true,
            message: "已刷新".into(),
            details: Some("3 条记录".into()),
            persist_secrets: None,
            authorization_url: None,
            resume_token: None,
            pending: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"ok":true,"message":"已刷新","details":"3 条记录"}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginActionResponse>(&json).unwrap(),
            resp
        );

        // details None 线形态为 null（规格 `string|null`，不做键省略）。
        let resp = PluginActionResponse::failure("不支持".into());
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"ok":false,"message":"不支持","details":null}"#
        );
        assert!(PluginActionResponse::success("ok".into()).ok);

        // persist_secrets：None 线形态省略键（旧插件响应零变化）；
        // Some 时逐键序列化，宿主据此合并进用户配置密文。
        let mut persist = std::collections::BTreeMap::new();
        persist.insert("session_key".into(), "abc".into());
        let resp = PluginActionResponse {
            ok: true,
            message: "已授权".into(),
            details: None,
            persist_secrets: Some(persist),
            authorization_url: None,
            resume_token: None,
            pending: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"ok":true,"message":"已授权","details":null,"persist_secrets":{"session_key":"abc"}}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginActionResponse>(&json).unwrap(),
            resp
        );

        let pending = PluginActionResponse::pending_authorization(
            "请授权".into(),
            "https://www.last.fm/api/auth/?api_key=k&token=t".into(),
            "t".into(),
        );
        let json = serde_json::to_string(&pending).unwrap();
        assert_eq!(
            json,
            r#"{"ok":true,"message":"请授权","details":null,"authorization_url":"https://www.last.fm/api/auth/?api_key=k&token=t","resume_token":"t","pending":true}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginActionResponse>(&json).unwrap(),
            pending
        );
        assert_eq!(
            serde_json::to_string(&PluginActionResponse::pending_wait("等待".into())).unwrap(),
            r#"{"ok":true,"message":"等待","details":null,"pending":true}"#
        );
    }

    #[test]
    fn plugin_error_network_retryable_independent() {
        let retry = PluginError::network("timeout".into(), true);
        assert!(retry.retryable);
        let no_retry = PluginError::network("bad gateway".into(), false);
        assert!(!no_retry.retryable);
    }

    /// `retry_after_secs` 线形态兼容：缺省省略键（旧插件 JSON 逐字节不变），
    /// 有值时携带；两种形态宿主都解析。历史响应形状由既有固定 JSON 断言锁定。
    #[test]
    fn plugin_error_retry_after_backwards_compatible() {
        // 旧插件响应（无该键）：解析成功且缺省为 None，错误码语义不变。
        let legacy = r#"{"code":"rate_limited","message":"upstream 429","retryable":true}"#;
        let parsed: PluginError = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.code, PluginErrorCode::RateLimited);
        assert_eq!(parsed.retry_after_secs, None);

        // 新插件响应：有值时输出键，且能原样往返。
        let with_secs = PluginError::rate_limited(
            "upstream 429".into(),
            RateLimitRetry {
                retry_after_secs: Some(17),
                retry_scope: None,
            },
        );
        let json = serde_json::to_string(&with_secs).unwrap();
        assert_eq!(
            json,
            r#"{"code":"rate_limited","message":"upstream 429","retryable":true,"retry_after_secs":17}"#
        );
        assert_eq!(
            serde_json::from_str::<PluginError>(&json).unwrap(),
            with_secs
        );

        // 无 Retry-After 的限流与构建器两种写法等价。
        assert_eq!(
            PluginError::rate_limited("upstream 429".into(), RateLimitRetry::default(),),
            PluginError::new(PluginErrorCode::RateLimited, "upstream 429".into())
        );
        assert_eq!(
            PluginError::new(PluginErrorCode::RateLimited, "upstream 429".into()).with_retry(
                RateLimitRetry {
                    retry_after_secs: Some(17),
                    retry_scope: None,
                }
            ),
            with_secs
        );
    }

    /// `retry_scope` 与 `retry_after_secs` 同口径：缺省省略键，新旧 JSON 都能解析。
    #[test]
    fn plugin_error_retry_scope_backwards_compatible() {
        let legacy = r#"{"code":"rate_limited","message":"upstream 429","retryable":true}"#;
        let parsed: PluginError = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.retry_scope, None);

        let unknown: PluginError = serde_json::from_str(
            r#"{"code":"rate_limited","message":"upstream 429","retryable":true,"future_field":1}"#,
        )
        .unwrap();
        assert_eq!(unknown.retry_scope, None);

        let scoped = PluginError::rate_limited(
            "cool".into(),
            RateLimitRetry {
                retry_after_secs: Some(9),
                retry_scope: Some("org.example.api".into()),
            },
        );
        let json = serde_json::to_string(&scoped).unwrap();
        assert_eq!(
            json,
            r#"{"code":"rate_limited","message":"cool","retryable":true,"retry_after_secs":9,"retry_scope":"org.example.api"}"#
        );
        assert_eq!(serde_json::from_str::<PluginError>(&json).unwrap(), scoped);
    }

    #[test]
    fn all_error_codes_roundtrip_flat_snake_case() {
        for (code, expect) in [
            (PluginErrorCode::Network, r#""network""#),
            (PluginErrorCode::RateLimited, r#""rate_limited""#),
            (PluginErrorCode::InvalidArgument, r#""invalid_argument""#),
            (PluginErrorCode::Unsupported, r#""unsupported""#),
            (PluginErrorCode::PermanentFailure, r#""permanent_failure""#),
            (PluginErrorCode::Internal, r#""internal""#),
        ] {
            assert_eq!(serde_json::to_string(&code).unwrap(), expect);
            assert_eq!(
                serde_json::from_str::<PluginErrorCode>(expect).unwrap(),
                code
            );
        }
    }

    #[test]
    fn full_call_roundtrip_scrape() {
        let req = PluginRequest {
            id: 42,
            op: PluginOp::Scrape {
                query: EntityQuery {
                    kind: ScrapeEntityKind::Album,
                    mbid: None,
                    isrc: None,
                    name: Some("碟".into()),
                    artist_name: Some("人".into()),
                    known_external_ids: Vec::new(),
                    library_types: Vec::new(),
                },
            },
        };
        let wire = serde_json::to_string(&req).unwrap();
        let req2: PluginRequest = serde_json::from_str(&wire).unwrap();
        let PluginOp::Scrape { query } = req2.op else {
            panic!("op 必须反序列化回 scrape");
        };
        assert_eq!(query.kind, ScrapeEntityKind::Album);

        let resp = PluginResponse {
            id: req2.id,
            outcome: PluginOutcome::Scrape(Box::new(ScrapeResult {
                confidence: 1.0,
                ..ScrapeResult::default()
            })),
        };
        let wire = serde_json::to_string(&resp).unwrap();
        let resp2: PluginResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(resp2.id, 42);
        let PluginOutcome::Scrape(result) = resp2.outcome else {
            panic!("outcome 必须反序列化回 scrape");
        };
        assert_eq!(result.confidence, 1.0);
    }
}
