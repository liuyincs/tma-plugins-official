//! 宿主 `ai_chat` 函数跨边界 JSON 协议（ABI 1.4+）。
//!
//! 字段名与线格式稳定：guest 经 extism host fn 传 offset，载荷为 UTF-8 JSON。
//! 风格与 [`crate::http_host`] 对齐（`http_request` ↔ `ai_chat`）：请求是插件
//! 写死的提示词，响应是 `{ok, content?, error?, code?}`。
//!
//! **语义约束（宿主实现与插件作者都必须遵守）**：
//!
//! - **单轮、非连续对话**：宿主不保留任何会话状态，每次调用独立计费、独立
//!   上下文。插件要"多轮"必须自行把前文拼进 prompt 重新发送；
//! - **插件自写 system/user prompt**：宿主不追加任何系统提示词，也不做模板
//!   包装——插件对提示词内容与输出格式全权负责；
//! - **`max_tokens` 由核心写死、插件不可指定**：本 DTO 刻意不含该字段，配额
//!   属宿主统一治理（防止第三方插件把共享凭据当免费算力）。未来若开放，
//!   只能以 ABI major 升级的方式加字段回来。
//!
//! 权限边界：调用 `ai_chat` 前置要求清单声明 [`crate::permission::Permission::Ai`]
//! （宿主闸门逐次校验，未声明即拒绝）。
//!
//! **稳定错误码**（失败形态的 `code` 字段，插件可编程区分；`error` 是人读文案，
//! 文案措辞宿主可调整，`code` 不可）：
//!
//! | `code` | 语义 |
//! |--------|------|
//! | `forbidden` | 清单未声明 `Permission::Ai`，或调用栈未被宿主授权 AI（scrape / `tma_event` 等非用户点击路径构造时不注入回调） |
//! | `prompt_too_long` | system+user 总字节数超宿主上限（**不截断，直接拒**，不消耗次数） |
//! | `budget_exhausted` | 本次用户操作的调用次数预算已用尽（下次操作重新计数） |
//! | `timeout` | 本次操作的墙钟总超时已到（与次数预算相互独立） |
//! | `ai_not_configured` | 核心 AI 未配置（base_url / model / api_key 任一缺失） |
//! | `upstream_error` | 上游 chat 调用失败（网络 / 4xx / 5xx 重试耗尽 / 空 content） |
//! | `bad_request` / `internal` | 入参形态非法 / 宿主侧意外失败（与 `http_request` 同口径） |

use serde::{Deserialize, Serialize};

/// 插件→宿主 AI 对话请求（`ai_chat` 入参）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiChatHostRequest {
    /// system 提示词（可选：插件未提供时宿主不注入任何系统提示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// user 提示词（必填；单轮对话的正文）。
    pub user: String,
}

/// 宿主→插件 AI 对话响应（`ai_chat` 出参）。
///
/// 成功：`ok: true` + `content`（模型首选项文本）；
/// 失败：`ok: false` + `error` / `code`（`content` 可缺省）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiChatHostResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl AiChatHostResponse {
    /// 构造成功响应（`content` 为模型输出文本）。
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: Some(content.into()),
            error: None,
            code: None,
        }
    }

    /// 构造失败响应（`code` 为宿主侧稳定错误码，`error` 为人读描述）。
    pub fn failure(code: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: None,
            error: Some(error.into()),
            code: Some(code.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_shape() {
        let req = AiChatHostRequest {
            system: Some("只输出 JSON".into()),
            user: "从下面的文本抽取曲目：…".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"system":"只输出 JSON","user":"从下面的文本抽取曲目：…"}"#
        );
        assert_eq!(
            serde_json::from_str::<AiChatHostRequest>(&json).unwrap(),
            req
        );
    }

    /// system 缺省时省略该键（线形态最小）；`user` 缺失即解析失败。
    #[test]
    fn request_requires_user_and_omits_absent_system() {
        let req: AiChatHostRequest = serde_json::from_str(r#"{"user":"hello"}"#).unwrap();
        assert_eq!(req.system, None);
        assert_eq!(req.user, "hello");

        assert!(serde_json::from_str::<AiChatHostRequest>(r#"{"system":"s"}"#).is_err());
        let json = serde_json::to_string(&AiChatHostRequest {
            system: None,
            user: "hi".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"user":"hi"}"#);
    }

    /// 成功形态：`ok` 恒出现；`content` 有值时输出，`error`/`code` 省略。
    #[test]
    fn response_ok_and_err_shapes() {
        let ok = AiChatHostResponse::success(r#"[{"title":"Song"}]"#);
        let json = serde_json::to_string(&ok).unwrap();
        assert_eq!(json, r#"{"ok":true,"content":"[{\"title\":\"Song\"}]"}"#);

        let err = AiChatHostResponse::failure("ai_not_configured", "AI 未配置");
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"ok":false,"error":"AI 未配置","code":"ai_not_configured"}"#
        );
    }

    /// 旧/宽松形态兼容：裸 `{"ok":true}` 也能解析（字段全部缺省）。
    #[test]
    fn response_deserializes_minimal_shape() {
        let resp: AiChatHostResponse = serde_json::from_str(r#"{"ok":false}"#).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.content, None);
        assert_eq!(resp.error, None);
        assert_eq!(resp.code, None);
    }
}
