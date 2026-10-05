use super::*;
use crate::message::RateLimitRetry;

// ------------------------- percent 编码 -------------------------

#[test]
fn percent_encode_keeps_unreserved_and_encodes_space() {
    assert_eq!(percent_encode_query("The Beatles"), "The%20Beatles");
    assert_eq!(percent_encode_query("AZaz09-_.~"), "AZaz09-_.~");
}

#[test]
fn percent_encode_cjk_bytes_uppercase_hex() {
    // 周杰伦 = E5 91 A8 / E6 9D B0 / E4 BC A6
    assert_eq!(
        percent_encode_query("周杰伦"),
        "%E5%91%A8%E6%9D%B0%E4%BC%A6"
    );
}

#[test]
fn percent_encode_encodes_reserved_characters() {
    assert_eq!(percent_encode_query("a&b=c?d/e#f"), "a%26b%3Dc%3Fd%2Fe%23f");
    assert_eq!(percent_encode_query("100%+"), "100%25%2B");
    // RFC 3986 查询组件：空格是 %20，不是表单编码的 +。
    assert_eq!(percent_encode_query("a b"), "a%20b");
    assert_eq!(percent_encode_query(""), "");
}

// ------------------------- 状态映射 -------------------------

#[test]
fn status_429_maps_to_rate_limited_retryable() {
    let e = status_to_error(429, "slow down");
    assert_eq!(e.code, PluginErrorCode::RateLimited);
    assert!(e.retryable);
    assert!(e.message.contains("429"));
    assert!(e.message.contains("slow down"));
}

#[test]
fn status_5xx_maps_to_retryable_network() {
    for status in [500u16, 502, 503, 599] {
        let e = status_to_error(status, "");
        assert_eq!(e.code, PluginErrorCode::Network, "HTTP {status}");
        assert!(e.retryable, "HTTP {status} 应可重试");
    }
}

#[test]
fn status_404_and_403_map_to_permanent_failure() {
    for status in [400u16, 401, 403, 404, 410, 422] {
        let e = status_to_error(status, "no such entity");
        assert_eq!(e.code, PluginErrorCode::PermanentFailure, "HTTP {status}");
        assert!(!e.retryable);
    }
    assert!(status_to_error(403, "").message.contains("403"));
}

#[test]
fn status_hint_is_truncated_on_char_boundary() {
    let long = "错".repeat(300);
    let e = status_to_error(500, &long);
    assert!(e.message.chars().count() < 200, "超长 body 提示需截断");
    // 1xx/2xx 是调用方误用：Internal 而非伪装成上游错误。
    assert_eq!(status_to_error(200, "").code, PluginErrorCode::Internal);
}

// ------------------------- base64 -------------------------

#[test]
fn base64_known_vectors() {
    assert_eq!(base64_encode(b"hi"), "aGk=");
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(base64_encode(b"hell"), "aGVsbA==");
    assert_eq!(base64_encode(b""), "");
    // CJK：与宿主 base64::STANDARD 的输出互通（向量与系统 base64 对拍固定）。
    assert_eq!(base64_encode("周杰伦".as_bytes()), "5ZGo5p2w5Lym");
    assert_eq!(base64_decode("aGk=").as_deref(), Some(b"hi".as_slice()));
    assert_eq!(
        base64_decode("aGVsbG8=").as_deref(),
        Some(b"hello".as_slice())
    );
    assert_eq!(
        base64_decode("5ZGo5p2w5Lym").as_deref(),
        Some("周杰伦".as_bytes())
    );
    assert_eq!(base64_decode("").as_deref(), Some(Vec::new().as_slice()));
    // 容忍空白。
    assert_eq!(base64_decode("aGk\n =").as_deref(), Some(b"hi".as_slice()));
}

#[test]
fn base64_rejects_malformed_input() {
    assert!(base64_decode("A").is_none(), "长度非 4 的倍数");
    assert!(base64_decode("A===").is_none(), "padding 超限");
    assert!(base64_decode("a=Gk=").is_none(), "padding 不在尾部");
    assert!(base64_decode("aG*k").is_none(), "非法字母");
}

#[test]
fn base64_roundtrip_various_lengths() {
    for len in 0..40 {
        let data: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
        let encoded = base64_encode(&data);
        assert_eq!(encoded.len() % 4, 0);
        assert_eq!(base64_decode(&encoded).as_deref(), Some(data.as_slice()));
    }
}

// ------------------------- GuestHttp -------------------------

fn ok_json_transport(
    status: u16,
    body: &str,
) -> impl Fn(HttpHostRequest) -> Result<HttpHostResponse, PluginError> {
    let body = body.to_string();
    move |_| {
        Ok(HttpHostResponse::success(
            status,
            BTreeMap::new(),
            base64_encode(body.as_bytes()),
        ))
    }
}

#[test]
fn get_json_parses_body() {
    let t = ok_json_transport(200, r#"{"entities":{"Q1299":{}}}"#);
    let v = GuestHttp::get_json(&t, "https://www.wikidata.org/w/api.php", BTreeMap::new()).unwrap();
    assert!(v["entities"]["Q1299"].is_object());
}

#[test]
fn get_json_empty_body_is_null() {
    for body in ["", "  "] {
        let t = ok_json_transport(204, body);
        let v = GuestHttp::get_json(&t, "https://example.com", BTreeMap::new()).unwrap();
        assert!(v.is_null(), "空 body 应为 Null（{body:?}）");
    }
    // body_b64 缺失同样为 Null。
    let t = |_req: HttpHostRequest| {
        Ok(HttpHostResponse {
            ok: true,
            status: 200,
            headers: BTreeMap::new(),
            body_b64: None,
            error: None,
            code: None,
            retry_after_secs: None,
            retry_scope: None,
        })
    };
    assert!(
        GuestHttp::get_json(&t, "u", BTreeMap::new())
            .unwrap()
            .is_null()
    );
}

#[test]
fn get_json_maps_non_2xx_status() {
    let t = ok_json_transport(429, "{\"error\":\"rate\"}");
    let e = GuestHttp::get_json(&t, "https://example.com", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::RateLimited);
    assert!(e.message.contains("rate"), "body 提示应进错误消息");

    let t = ok_json_transport(503, "");
    let e = GuestHttp::get_json(&t, "https://example.com", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Network);
    assert!(e.retryable);
}

/// 429 + delta-seconds Retry-After：秒数附进 `retry_after_secs`；
/// 非法/0 有退避；429 无头 → None（走管线自身退避）。
#[test]
fn get_json_429_carries_retry_after_seconds() {
    let t = ok_json_transport(429, "");
    let e = GuestHttp::get_json(&t, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.retry_after_secs, None, "无头不造数");

    let with_header = |header: &str| {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_string(), header.to_string());
        let t = move |_req: HttpHostRequest| {
            Ok(HttpHostResponse::success(
                429,
                headers.clone(),
                base64_encode(b"{}"),
            ))
        };
        GuestHttp::get_json(&t, "u", BTreeMap::new()).unwrap_err()
    };
    assert_eq!(with_header("30").retry_after_secs, Some(30));
    assert_eq!(
        with_header("0").retry_after_secs,
        Some(MIN_RETRY_AFTER_SECS)
    );
    assert_eq!(with_header("junk").retry_after_secs, None);
}

#[test]
fn retry_after_seconds_only_parses_integers() {
    assert_eq!(parse_retry_after_seconds(Some("30")), Some(30));
    assert_eq!(parse_retry_after_seconds(Some(" 7 ")), Some(7));
    assert_eq!(
        parse_retry_after_seconds(Some("0")),
        Some(MIN_RETRY_AFTER_SECS)
    );
    assert_eq!(
        parse_retry_after_seconds(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
        None
    );
    assert_eq!(parse_retry_after_seconds(None), None);
}

#[test]
fn get_json_maps_host_failures() {
    let network = |_req: HttpHostRequest| Ok(HttpHostResponse::failure("network", "dns boom"));
    let e = GuestHttp::get_json(&network, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Network);
    assert!(e.retryable);
    assert!(e.message.contains("dns boom"));

    let forbidden = |_req: HttpHostRequest| Ok(HttpHostResponse::failure("forbidden", "denied"));
    let e = GuestHttp::get_json(&forbidden, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::PermanentFailure);
    assert!(!e.retryable);
    assert!(e.message.contains("denied"));
}

#[test]
fn get_json_rejects_non_json_success_body() {
    let t = ok_json_transport(200, "<html>not json</html>");
    let e = GuestHttp::get_json(&t, "https://example.com", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Internal);
    assert!(e.message.contains("JSON"));
}

#[test]
fn request_json_carries_method_headers_and_body() {
    let seen = std::cell::RefCell::new(None);
    let t = |req: HttpHostRequest| {
        *seen.borrow_mut() = Some(req);
        Ok(HttpHostResponse::success(
            200,
            BTreeMap::new(),
            base64_encode(br#"{"ok":true}"#),
        ))
    };
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    let v = GuestHttp::request_json(
        &t,
        "POST",
        "https://accounts.spotify.com/api/token",
        headers,
        Some(br#"{"grant_type":"client_credentials"}"#),
    )
    .unwrap();
    assert_eq!(v["ok"], true);

    let req = seen.borrow().as_ref().unwrap().clone();
    assert_eq!(req.method, "POST");
    assert_eq!(req.url, "https://accounts.spotify.com/api/token");
    assert_eq!(req.headers["content-type"], "application/json");
    assert_eq!(
        base64_decode(req.body_b64.as_deref().unwrap()).as_deref(),
        Some(br#"{"grant_type":"client_credentials"}"#.as_slice())
    );
}

#[test]
fn transport_error_propagates() {
    let t = |_req: HttpHostRequest| {
        Err(PluginError::new(
            PluginErrorCode::Internal,
            "宿主函数调用失败".into(),
        ))
    };
    let e = GuestHttp::get_json(&t, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Internal);
    assert_eq!(e.message, "宿主函数调用失败");
}

// ------------------------- request_raw -------------------------

/// raw 路径：状态码/响应头/原始字节原样透传，非 2xx 也不做状态分类。
#[test]
fn request_raw_passes_through_status_headers_and_bytes() {
    let mut resp_headers = BTreeMap::new();
    resp_headers.insert("retry-after".to_string(), "17".to_string());
    let t = move |_req: HttpHostRequest| {
        Ok(HttpHostResponse::success(
            503,
            resp_headers.clone(),
            base64_encode(b"rate limited"),
        ))
    };
    let raw = GuestHttp::request_raw(
        &t,
        "GET",
        "https://musicbrainz.org/ws/2/artist/x",
        BTreeMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(raw.status, 503, "非 2xx 原样透传，不分类");
    assert_eq!(
        raw.headers.get("retry-after").map(String::as_str),
        Some("17")
    );
    assert_eq!(raw.body, b"rate limited");
    assert_eq!(raw.body_text_lossy(), "rate limited");
}

/// raw 路径：body 缺失 → 空 Vec；请求 method/headers/body 照常携带。
#[test]
fn request_raw_missing_body_is_empty_and_carries_request() {
    let seen = std::cell::RefCell::new(None);
    let t = |req: HttpHostRequest| {
        *seen.borrow_mut() = Some(req);
        Ok(HttpHostResponse {
            ok: true,
            status: 200,
            headers: BTreeMap::new(),
            body_b64: None,
            error: None,
            code: None,
            retry_after_secs: None,
            retry_scope: None,
        })
    };
    let raw = GuestHttp::request_raw(
        &t,
        "POST",
        "https://example.com/token",
        BTreeMap::new(),
        Some(b"grant_type=client_credentials"),
    )
    .unwrap();
    assert_eq!(raw.status, 200);
    assert!(raw.body.is_empty(), "body_b64 缺失 → 空 Vec");

    let req = seen.borrow().as_ref().unwrap().clone();
    assert_eq!(req.method, "POST");
    assert_eq!(
        base64_decode(req.body_b64.as_deref().unwrap()).as_deref(),
        Some(b"grant_type=client_credentials".as_slice())
    );
}

/// raw 路径：`ok:false` 宿主失败仍走统一映射（与 request_json 同一份）。
#[test]
fn request_raw_maps_host_failures() {
    let network = |_req: HttpHostRequest| Ok(HttpHostResponse::failure("network", "dns boom"));
    let e = GuestHttp::request_raw(&network, "GET", "u", BTreeMap::new(), None).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Network);
    assert!(e.retryable);
    assert!(e.message.contains("dns boom"));

    let forbidden = |_req: HttpHostRequest| Ok(HttpHostResponse::failure("forbidden", "denied"));
    let e = GuestHttp::request_raw(&forbidden, "GET", "u", BTreeMap::new(), None).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::PermanentFailure);
    assert!(!e.retryable);
}

/// 宿主来源冷却的本地拒绝（`rate_limited`）映射为可重试限流错误，
/// 且宿主给出的退避秒数原样保留（不得伪装成上游真实响应）。
#[test]
fn host_cooldown_rejection_maps_to_rate_limited_with_retry() {
    let cooling = |_req: HttpHostRequest| {
        Ok(HttpHostResponse::failure_with_retry(
            "rate_limited",
            "source cooling down",
            RateLimitRetry {
                retry_after_secs: Some(42),
                retry_scope: None,
            },
        ))
    };
    let e = GuestHttp::get_json(&cooling, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::RateLimited);
    assert!(e.retryable);
    assert_eq!(e.retry_after_secs, Some(42));
    assert_eq!(e.retry_scope, None);

    let scoped = |_req: HttpHostRequest| {
        Ok(HttpHostResponse::failure_with_retry(
            "rate_limited",
            "source cooling down",
            RateLimitRetry {
                retry_after_secs: Some(7),
                retry_scope: Some("org.example.api".into()),
            },
        ))
    };
    let e = GuestHttp::get_json(&scoped, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.retry_scope.as_deref(), Some("org.example.api"));
    assert_eq!(e.retry_after_secs, Some(7));

    // 无退避字段的冷却拒绝：仍为限流，秒数缺省。
    let plain = |_req: HttpHostRequest| Ok(HttpHostResponse::failure("rate_limited", "cool"));
    let e = GuestHttp::get_json(&plain, "u", BTreeMap::new()).unwrap_err();
    assert_eq!(e.code, PluginErrorCode::RateLimited);
    assert_eq!(e.retry_after_secs, None);
}

// ------------------------- Retry-After -------------------------

#[test]
fn retry_after_seconds_and_missing() {
    let now = 1_700_000_000;
    assert_eq!(parse_retry_after(None, now), None, "头缺失 → None");
    assert_eq!(parse_retry_after(Some(""), now), None, "空白 → None");
    assert_eq!(parse_retry_after(Some(" 17 "), now), Some(17));
    assert_eq!(
        parse_retry_after(Some("0"), now),
        Some(MIN_RETRY_AFTER_SECS),
        "0 → 最小退避，不忙循环"
    );
    assert_eq!(
        parse_retry_after(Some("next tuesday"), now),
        None,
        "完全非法 → None（走调用方退避）"
    );
}

#[test]
fn retry_after_http_date() {
    // IMF-fixdate：Wed, 21 Oct 2015 07:28:00 GMT = 1445412480。
    let date = "Wed, 21 Oct 2015 07:28:00 GMT";
    assert_eq!(parse_http_date_unix(date), Some(1_445_412_480));
    // 过去日期 → 剩余 0 → 最小退避。
    assert_eq!(
        parse_retry_after(Some(date), 1_700_000_000),
        Some(MIN_RETRY_AFTER_SECS)
    );
    // 未来日期 → 剩余秒数。
    let future = "Wed, 21 Oct 2115 07:28:00 GMT";
    let got = parse_retry_after(Some(future), 1_700_000_000).unwrap();
    assert!(got > 60, "未来日期应给出可观的等待秒数: {got}");
    // 非法日期 → None。
    assert_eq!(parse_http_date_unix("Wed, 21 Foo 2015 07:28:00 GMT"), None);
    assert_eq!(parse_http_date_unix("Mon, 32 Jan 2034 07:28:00 GMT"), None);
    assert_eq!(parse_http_date_unix("21 Oct 2015"), None);
}

/// raw 路径：body base64 非法 → Internal（宿主协议违约，不属于状态分类）。
#[test]
fn request_raw_rejects_malformed_base64_body() {
    let t = |_req: HttpHostRequest| {
        Ok(HttpHostResponse::success(
            200,
            BTreeMap::new(),
            "!!not-b64!!",
        ))
    };
    let e = GuestHttp::request_raw(&t, "GET", "https://example.com", BTreeMap::new(), None)
        .unwrap_err();
    assert_eq!(e.code, PluginErrorCode::Internal);
    assert!(e.message.contains("base64"));
}

/// request_json 行为不变：薄层语义（2xx 分类 + JSON 解析 + 空 body → Null）
/// 由既有 get_json_* / request_json_* 测试锁死；此处只补一条「错误 hint 取自
/// raw body」的对照。
#[test]
fn request_json_error_hint_comes_from_raw_body() {
    let t = ok_json_transport(403, "forbidden body");
    let e = GuestHttp::request_json(&t, "GET", "https://example.com", BTreeMap::new(), None)
        .unwrap_err();
    assert_eq!(e.code, PluginErrorCode::PermanentFailure);
    assert!(e.message.contains("forbidden body"));
}

// ------------------------- load_config -------------------------

#[derive(Debug, serde::Deserialize, Default, PartialEq)]
struct FakeConfig {
    #[serde(default)]
    api_key: String,
    #[serde(default = "default_language")]
    language: String,
}

fn default_language() -> String {
    "zh".to_string()
}

#[test]
fn load_config_fills_defaults_and_reads_values() {
    let empty: FakeConfig = load_config("{}");
    assert_eq!(
        empty,
        FakeConfig {
            api_key: String::new(),
            language: "zh".into()
        }
    );
    assert_eq!(load_config::<FakeConfig>(""), empty);
    assert_eq!(load_config::<FakeConfig>("   "), empty);

    let cfg: FakeConfig = load_config(r#"{"api_key":"k","language":"ja"}"#);
    assert_eq!(cfg.api_key, "k");
    assert_eq!(cfg.language, "ja");

    // 部分字段：其余走 serde 默认。
    let cfg: FakeConfig = load_config(r#"{"api_key":"k"}"#);
    assert_eq!(cfg.language, "zh");
}
