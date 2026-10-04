//! Guest 侧共享工具：出站 HTTP 调用封装、运行时配置解析与 URL 编码。
//!
//! 本模块同时编译进宿主（普通 `cargo test` 单测）与 wasm guest（内置刮削插件），
//! 因此**只允许纯 serde 逻辑**：不声明任何 extism 宿主函数、不依赖 tokio/reqwest。
//! 插件自行用 `#[host_fn("extism:host/user")] extern "ExtismHost"` 声明
//! `http_request` / `tma_config`，再把调用包装成 [`HttpTransport`] 闭包喂进来
//! （内置 8 插件的这份 FFI 样板已收敛到 `tma-plugin-sdk` 的 `scrape_plugin!` 宏）：
//!
//! ```ignore
//! # use extism_pdk::Json;
//! # use tma_plugin_sdk::{guest::GuestHttp, HttpHostRequest, HttpHostResponse, PluginError};
//! #[host_fn("extism:host/user")]
//! extern "ExtismHost" {
//!     fn http_request(input: Json<HttpHostRequest>) -> Json<HttpHostResponse>;
//! }
//!
//! let transport = |req: HttpHostRequest| -> Result<HttpHostResponse, PluginError> {
//!     unsafe { http_request(Json(req)) }
//!         .map(|Json(resp)| resp)
//!         .map_err(|e| PluginError::new(PluginError::internal_code(), format!("{e:?}")))
//! };
//! let value = GuestHttp::get_json(&transport, url, headers)?;
//! ```
//!
//! base64 为零依赖手写实现（标准字母表 + padding）：`tma-plugin-sdk` 刻意不引
//! base64 crate，宿主与 guest 的编解码语义在此收敛并经单测锁死。

use std::collections::BTreeMap;

use serde_json::Value;

use crate::TrackEventFields;
use crate::http_host::{HttpHostRequest, HttpHostResponse};
use crate::message::{PluginError, PluginErrorCode, RateLimitRetry};

/// 过短曲目不上报（Last.fm / ListenBrainz 等 scrobble 插件共用惯例：< 30s 跳过）。
pub const MIN_SCROBBLE_SECONDS: u64 = 30;

/// 多艺术家单串口径：primary + collaborators 以 `", "` 拼接（见 [`crate::playlist_import::join_artist_names`]）。
pub fn join_artists(track: &TrackEventFields) -> String {
    let mut names = Vec::new();
    let primary = track.primary_artist.trim();
    if !primary.is_empty() {
        names.push(primary.to_string());
    }
    for name in &track.artists {
        let name = name.trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    crate::playlist_import::join_artist_names(names).unwrap_or_default()
}

/// <30s 跳过判据：时长未知（None）不跳过。
pub fn should_skip_scrobble(duration_seconds: Option<u64>) -> bool {
    matches!(duration_seconds, Some(d) if d < MIN_SCROBBLE_SECONDS)
}

/// 原生管线 `FetchCtx.user_agent` 的硬编码值（宿主侧权威在
/// `tma-plugin-runtime` 的 client 模块）。guest 无法依赖宿主 crate，
/// 同一字面量在此收敛一份：插件一律引用本常量，不再各自抄写 `"TMA/0.1.0"`。
/// （MusicBrainz 插件另需拼联系方式后缀，仍以本常量为基础。）
pub const DEFAULT_USER_AGENT: &str = "TMA/0.1.0";

/// 出站传输函数：插件把 extism `http_request` 宿主函数包成该形状喂给 [`GuestHttp`]。
///
/// 同步签名（wasm 内宿主函数调用本就是同步 FFI）；单测用纯闭包伪造，
/// 不需要 wasm 运行时即可全链路测试。
pub type HttpTransport<'a> = &'a dyn Fn(HttpHostRequest) -> Result<HttpHostResponse, PluginError>;

// ---------------------------------------------------------------------------
// URL 查询组件编码（RFC 3986）
// ---------------------------------------------------------------------------

/// 百分号编码查询组件：非保留字符 `A-Za-z0-9-_.~` 之外全部 `%XX`（大写十六进制）。
///
/// 与表单编码不同，空格编码为 `%20` 而非 `+`（Wikidata/Wikipedia 查询参数语义）。
pub fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0x0f));
            }
        }
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + nibble - 10) as char,
    }
}

// ---------------------------------------------------------------------------
// 状态码 → PluginError（guest 侧统一口径）
// ---------------------------------------------------------------------------

/// 非 2xx 状态码的 guest 侧统一投影（宿主侧原生的 classify_http_status 已随
/// 内置来源插件化移除，本函数即唯一口径）：
///
/// - 429 → `RateLimited`（retryable）；
/// - 5xx → `Network` 且 `retryable = true`（上游临时故障，可退避重试）；
/// - 其余 3xx/4xx → `PermanentFailure`（重试无意义）；
/// - 1xx/2xx 落到这里是调用方误用，报 `Internal`。
///
/// `body_hint` 为响应体开头片段（可空），仅用于人读消息。
/// 注意：MusicBrainz 用 503 表达限流，其插件需在调用本函数前自行特判
/// （对齐原生 `classify_mb_status`）。
pub fn status_to_error(status: u16, body_hint: &str) -> PluginError {
    let hint = truncate_hint(body_hint);
    let message = if hint.is_empty() {
        format!("上游 HTTP {status}")
    } else {
        format!("上游 HTTP {status}: {hint}")
    };
    match status {
        429 => PluginError::new(PluginErrorCode::RateLimited, message),
        500..=599 => PluginError::network(message, true),
        300..=499 => PluginError::new(PluginErrorCode::PermanentFailure, message),
        _ => PluginError::new(
            PluginErrorCode::Internal,
            format!("非错误状态码被当作错误处理（HTTP {status}）"),
        ),
    }
}

/// 错误消息里的 body 提示截断：按字符边界最多 160 字符（防超长响应体撑爆日志/任务明细）。
///
/// `pub` 供需要自行分类状态码的插件复用（如 MusicBrainz 的 503 特判），
/// 避免各插件逐字克隆同一份截断逻辑。
pub fn truncate_hint(hint: &str) -> String {
    let hint = hint.trim();
    if hint.chars().count() <= 160 {
        return hint.to_string();
    }
    let cut: String = hint.chars().take(160).collect();
    format!("{cut}…")
}

/// Retry-After 头值 → 建议等待秒数（RFC 9110：delay-seconds 或 HTTP-date）。
///
/// 返回 `None` = 头缺失或完全非法（调用方按自身退避节奏处理）；
/// `Some(0)` 或已过去的日期 → [`MIN_RETRY_AFTER_SECS`]，保证有退避、不忙循环。
/// `now_unix` 由调用方提供（guest 无时钟，宿主侧传当前 Unix 秒即可）。
pub fn parse_retry_after(value: Option<&str>, now_unix: u64) -> Option<u64> {
    const MIN: u64 = MIN_RETRY_AFTER_SECS;
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    // delay-seconds：非负十进制整数。
    if let Some(secs) = raw
        .parse::<u64>()
        .ok()
        .or_else(|| parse_http_date_unix(raw).map(|at| at.saturating_sub(now_unix)))
    {
        return Some(secs.max(MIN));
    }
    None
}

/// Retry-After 缺失但限流成立时的最小退避秒数：有退避、不忙循环。
pub const MIN_RETRY_AFTER_SECS: u64 = 1;

/// 仅解析 delta-seconds 形态的 Retry-After（guest 无时钟，日期形态留给宿主侧
/// [`parse_retry_after`]）。非法/0 → 最小退避；非整数 → `None`。
pub fn parse_retry_after_seconds(value: Option<&str>) -> Option<u64> {
    let secs = value?.trim().parse::<u64>().ok()?;
    Some(secs.max(MIN_RETRY_AFTER_SECS))
}

/// 解析 IMF-fixdate（RFC 7231，如 `Wed, 21 Oct 2015 07:28:00 GMT`）为 Unix 秒。
///
/// 手写解析（纯 std）：插件与宿主共享此口径，无需引入时间库。
/// 只认 IMF-fixdate（Retry-After 等标准头的推荐形态）；其余格式 → `None`。
fn parse_http_date_unix(raw: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = raw.split_whitespace().collect();
    // 形态：`周几, DD Mon YYYY HH:MM:SS GMT`（周几与 GMT 仅做存在性校验）。
    if parts.len() != 6 {
        return None;
    }
    let day: u64 = parts[1].parse().ok()?;
    let month_idx = MONTHS
        .iter()
        .position(|m| parts[2].eq_ignore_ascii_case(m))?;
    let year: u64 = parts[3].parse().ok()?;
    let mut hms = parts[4].split(':');
    let (hour, minute, second) = (
        hms.next()?.parse::<u64>().ok()?,
        hms.next()?.parse::<u64>().ok()?,
        hms.next()?.parse::<u64>().ok()?,
    );
    if hour > 23 || minute > 59 || second > 60 || day == 0 || day > 31 {
        return None;
    }
    // 天数差转秒（忽略闰秒；civil-from-days 反推公式，见 Howard Hinnant 的日期算法）。
    let month_idx = month_idx as u64; // 0-based：Jan = 0。
    let y = year - u64::from(month_idx < 2); // Jan/Feb 归入上一年。
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = if month_idx >= 2 {
        month_idx - 2
    } else {
        month_idx + 10
    };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// `ok:false` 宿主响应 → [`PluginError`]（越权/网络失败的透传语义）。
///
/// `pub` 供绕过 [`GuestHttp`] 自行看状态码/响应头的插件复用
/// （如 MusicBrainz 503+Retry-After、Spotify 401/403、Wikipedia 404 回退），
/// 保证宿主失败码 → 错误码的映射全插件只有这一份。
pub fn host_failure_to_error(resp: &HttpHostResponse) -> PluginError {
    let detail = resp.error.as_deref().unwrap_or("unknown");
    match resp.code.as_deref() {
        Some("network") => {
            PluginError::network(format!("宿主出站请求失败（network）: {detail}"), true)
        }
        // 来源冷却中的本地拒绝：透传宿主给出的退避秒数（非上游真实响应）。
        Some("rate_limited") => PluginError::rate_limited(
            format!("宿主来源冷却中（rate_limited）: {detail}"),
            RateLimitRetry {
                retry_after_secs: resp.retry_after_secs,
                retry_scope: resp.retry_scope.clone(),
            },
        ),
        // 响应超宿主 body 上限：同一请求重试结果相同，按永久失败处理。
        Some("body_too_large") => PluginError::new(
            PluginErrorCode::PermanentFailure,
            format!("宿主截停出站响应（body_too_large）: {detail}"),
        ),
        Some("forbidden") => PluginError::new(
            PluginErrorCode::PermanentFailure,
            format!("宿主拒绝出站请求（forbidden）: {detail}"),
        ),
        other => PluginError::new(
            PluginErrorCode::Internal,
            format!(
                "宿主 http_request 调用失败（{}）: {detail}",
                other.unwrap_or("unknown")
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// GuestHttp：经宿主 http_request 的请求（raw 为底，JSON 为薄层）
// ---------------------------------------------------------------------------

/// [`GuestHttp::request_raw`] 的返回值：状态码、响应头与原始字节的透传，
/// 不做状态分类、不做 JSON 解析——由调用方自行决定非 2xx 的语义
/// （MusicBrainz 503 限流、Spotify 401/403、Wikipedia 404 语言回退等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResponse {
    /// HTTP 状态码（宿主失败时不会走到这里：`ok:false` 已映射为 [`PluginError`]）。
    pub status: u16,
    /// 响应头（宿主侧已归一化为小写键，同名多值取首个）。
    pub headers: BTreeMap<String, String>,
    /// 原始响应体字节（缺失/空 body 为空 `Vec`）。
    pub body: Vec<u8>,
}

impl RawResponse {
    /// body 的 UTF-8 有损文本视图（错误消息 hint / HTML 剥离等场景用）。
    pub fn body_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// 经宿主 `http_request` 出站的共享助手（无状态，全部为关联函数）。
///
/// [`request_raw`](GuestHttp::request_raw) 是底层入口；[`request_json`](GuestHttp::request_json)
/// /[`get_json`](GuestHttp::get_json) 是其上「期望 2xx + 解析 JSON」的薄封装。
pub struct GuestHttp;

impl GuestHttp {
    /// GET + 期望 2xx + 解析 JSON。空 body（缺失或空串）返回 `Value::Null`。
    pub fn get_json(
        transport: HttpTransport<'_>,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<Value, PluginError> {
        Self::request_json(transport, "GET", url, headers, None)
    }

    /// 任意 method 的原始请求：body 以 base64 装入 `body_b64`；返回
    /// [`RawResponse`]（状态/头/字节原样透传）。`ok:false` 的宿主失败仍走
    /// 统一映射（[`host_failure_to_error`]），状态码分类留给调用方。
    pub fn request_raw(
        transport: HttpTransport<'_>,
        method: &str,
        url: &str,
        headers: BTreeMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<RawResponse, PluginError> {
        let request = HttpHostRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body_b64: body.map(base64_encode),
        };
        let resp = transport(request)?;
        if !resp.ok {
            return Err(host_failure_to_error(&resp));
        }
        let body = match resp.body_b64.as_deref() {
            None => Vec::new(),
            Some(body_b64) => base64_decode(body_b64).ok_or_else(|| {
                PluginError::new(
                    PluginErrorCode::Internal,
                    format!("响应 body base64 解码失败（{method} {url}）"),
                )
            })?,
        };
        Ok(RawResponse {
            status: resp.status,
            headers: resp.headers,
            body,
        })
    }

    /// 任意 method 的 JSON 请求：[`request_raw`](Self::request_raw) 之上
    /// 叠加「期望 2xx（状态分类见 [`status_to_error`]）+ 解析 JSON」。
    /// 429 的 delta-seconds Retry-After 附进错误的 `retry_after_secs`。
    pub fn request_json(
        transport: HttpTransport<'_>,
        method: &str,
        url: &str,
        headers: BTreeMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<Value, PluginError> {
        let raw = Self::request_raw(transport, method, url, headers, body)?;
        if !(200..300).contains(&raw.status) {
            let mut error = status_to_error(raw.status, &raw.body_text_lossy());
            if raw.status == 429
                && let Some(secs) =
                    parse_retry_after_seconds(raw.headers.get("retry-after").map(String::as_str))
            {
                error = error.with_retry_after_secs(secs);
            }
            return Err(error);
        }
        // 空 body（缺失/空串/纯空白，如 204）→ Null，让调用方按字段缺失处理。
        if raw.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&raw.body).map_err(|e| {
            PluginError::new(
                PluginErrorCode::Internal,
                format!("响应不是合法 JSON（{method} {url}）: {e}"),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 运行时配置
// ---------------------------------------------------------------------------

/// 解析 `tma_config()` 宿主函数返回的配置 JSON 为目标结构体。
///
/// 宿主契约保证输入是 JSON（未配置为 `{}`）；本函数对空串/空白再做一次兜底。
/// 解析失败时回退空对象 `{}`，给配置结构体一次「全字段 `#[serde(default)]`」
/// 的机会——缺凭据的判定属于业务逻辑，不在这里炸掉。
///
/// # Panics
/// 输入与空对象都解析失败（配置结构体存在无默认值的必填字段，属插件作者错误）。
pub fn load_config<T: serde::de::DeserializeOwned>(config_json: &str) -> T {
    let trimmed = config_json.trim();
    let source = if trimmed.is_empty() { "{}" } else { trimmed };
    match serde_json::from_str::<T>(source) {
        Ok(config) => config,
        Err(e) => serde_json::from_str::<T>("{}").unwrap_or_else(|fallback| {
            panic!(
                "tma_config 解析失败（{e}），且空对象兜底也失败（{fallback}）：\
                 配置结构体字段请加 #[serde(default)]"
            )
        }),
    }
}

// ---------------------------------------------------------------------------
// base64（标准字母表 + padding，零依赖手写）
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准字母表 + `=` padding（与宿主 `base64::engine::general_purpose::STANDARD` 互通）。
///
/// `pub` 供自行拼认证头的插件复用（如 Spotify token 请求的
/// `Basic base64(client_id:client_secret)`），编解码语义全插件收敛于此。
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let acc = (b0 << 16) | (b1 << 8) | b2;
        let sextets = [
            (acc >> 18) & 0x3f,
            (acc >> 12) & 0x3f,
            (acc >> 6) & 0x3f,
            acc & 0x3f,
        ];
        let push = |out: &mut String, n: usize| out.push(B64_ALPHABET[sextets[n] as usize] as char);
        match chunk.len() {
            3 => {
                for i in 0..4 {
                    push(&mut out, i);
                }
            }
            2 => {
                for i in 0..3 {
                    push(&mut out, i);
                }
                out.push('=');
            }
            _ => {
                for i in 0..2 {
                    push(&mut out, i);
                }
                out.push_str("==");
            }
        }
    }
    out
}

/// 逆 [`base64_encode`]；容忍空白字符，拒绝长度/字母表/padding 非法输入（→ None）。
///
/// `pub` 与 [`base64_encode`] 配套，供自行处理 `body_b64` 的插件解码原始字节。
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let payload_end = cleaned
        .iter()
        .position(|&b| b == b'=')
        .unwrap_or(cleaned.len());
    let padding = cleaned.len() - payload_end;
    // 标准形态：总长（去空白）为 4 的倍数，padding 只在尾部且至多 2 个；
    // 末组有效字符至少 2 个（1 个字符不足以恢复任何字节）。
    if cleaned.len() % 4 != 0 || padding > 2 || payload_end % 4 == 1 {
        return None;
    }
    if cleaned[payload_end..].iter().any(|&b| b != b'=') {
        return None;
    }
    let payload = &cleaned[..payload_end];
    let mut out = Vec::with_capacity(payload.len() / 4 * 3 + 2);
    for chunk in payload.chunks(4) {
        let mut acc: u32 = 0;
        for &c in chunk {
            acc = (acc << 6) | b64_value(c)?;
        }
        // 低位补零对齐 24 bit，再按组内字符数取回 1/2/3 字节。
        acc <<= 6 * (4 - chunk.len()) as u32;
        let bytes = acc.to_be_bytes();
        match chunk.len() {
            4 => out.extend_from_slice(&bytes[1..4]),
            3 => out.extend_from_slice(&bytes[1..3]),
            2 => out.push(bytes[1]),
            _ => return None,
        }
    }
    Some(out)
}

fn b64_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
