//! 契约夹具：宿主 capability 宿主函数语义的 vendored 转写（`contract/
//! capability_host.json`，逐行手工转写自宿主 `capability_fns.rs`）+ 回放器。
//!
//! 回放器**不重写任何判断**：每条 case 新建记录型 [`CapabilityStub`] 与
//! [`HostFnState`]（`granted` 由 case 指定），把 JSON 输入按 SDK 线格式
//! 反序列化（字段缺省走 serde 默认，如 `version` 缺省 = 1）后直接打真实
//! 运行的 [`catalog_respond`]/[`identity_respond`]/[`media_respond`]，
//! 断言应答的 version/status/error code/message 与进入桩派发的次数——
//! testkit 宿主函数复刻相对宿主源的校验顺序与错误码偏差在这里显红。
//!
//! 覆盖边界（fixture `coverage`/`excluded` 字段为权威声明）：本夹具只覆盖
//! 宿主源里**不依赖 provider 实现与 wasm 内存协议**的段——DTO 解析、校验
//! 顺序与权限门；provider 未注入（`unavailable`）、provider 错误映射/panic、
//! 状态不可用属宿主侧行为，testkit 桩恒存在，不在转写范围。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Deserialize;

use crate::capability_stub::CapabilityStub;
use crate::host_fns::{
    HostFnState, catalog_respond, identity_respond, media_respond, parse_capability_input,
};
use crate::{
    CatalogMatch, CatalogReadRequest, CatalogReadResponse, IdentityReadRequest,
    IdentityReadResponse, MediaMatch, MediaStreamRequest, MediaStreamResponse, StubProxy,
};

/// vendored 契约夹具（转写自宿主 `capability_fns.rs`，provenance 记录来源
/// commit；宿主源变更时人工同步本文件并更新 commit）。
const FIXTURE_JSON: &str = include_str!("../contract/capability_host.json");

// ---------------------------------------------------------------------------
// fixture schema（serde derive；未知字段一律拒绝，schema 错误大声 panic）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractFixture {
    schema_version: u16,
    provenance: Provenance,
    coverage: Vec<String>,
    excluded: Vec<String>,
    host_cases: Vec<Case>,
    testkit_extensions: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    repository: String,
    path: String,
    commit: String,
    method: String,
    /// **宿主侧** SDK 版本（宿主源锁定的 `tma_plugin_sdk` 版本，当前
    /// 0.3.1；testkit 自身依赖锁 0.3.0）——两边不必同版，capability DTO
    /// 线格式一致即可，线格式漂移由 case 的 `version` 字面值断言兜住。
    host_sdk_version: String,
    capability_dto_version: u16,
}

/// 一条回放 case（`host_cases` 与 `testkit_extensions` 共用同一形状）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    #[serde(rename = "fn")]
    function: CapabilityFn,
    granted: Vec<String>,
    input: InputCase,
    #[serde(default)]
    stub: Option<StubCase>,
    expect: ExpectCase,
}

/// case 针对的三条 capability 宿主函数之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum CapabilityFn {
    #[serde(rename = "tma_catalog_read")]
    Catalog,
    #[serde(rename = "tma_identity_read")]
    Identity,
    #[serde(rename = "tma_media_stream")]
    Media,
}

/// case 输入：`json` = 正常 DTO（按 SDK 线格式反序列化，字段缺省走 serde
/// 默认）；`raw_json` = 任意 JSON 文本（反序列化失败路径）。untagged + 具体
/// struct 的 `deny_unknown_fields` 组合实现「按 kind 分派 + 未知字段拒绝」。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputCase {
    Json(JsonInput),
    RawJson(RawJsonInput),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonInput {
    /// 线上判别字面值 `"json"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: JsonKind,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonInput {
    /// 线上判别字面值 `"raw_json"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: RawJsonKind,
    value: String,
}

#[derive(Debug, Deserialize)]
enum JsonKind {
    #[serde(rename = "json")]
    Json,
}

#[derive(Debug, Deserialize)]
enum RawJsonKind {
    #[serde(rename = "raw_json")]
    RawJson,
}

/// case 的桩配置：缺省或 `strict_stub` = 全未配置（进派发即宿主级 `Err`）；
/// `orchestrated` = 编排响应（catalog/media 路由表 + identity 槽）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StubCase {
    Strict(StrictStub),
    Orchestrated(OrchestratedStub),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictStub {
    /// 线上判别字面值 `"strict_stub"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: StrictStubKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrchestratedStub {
    /// 线上判别字面值 `"orchestrated"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: OrchestratedKind,
    #[serde(default)]
    identity: Option<IdentityReadResponse>,
    #[serde(default)]
    catalog_routes: Vec<CatalogRouteFixture>,
    #[serde(default)]
    media_routes: Vec<MediaRouteFixture>,
}

#[derive(Debug, Deserialize)]
enum StrictStubKind {
    #[serde(rename = "strict_stub")]
    StrictStub,
}

#[derive(Debug, Deserialize)]
enum OrchestratedKind {
    #[serde(rename = "orchestrated")]
    Orchestrated,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRouteFixture {
    #[serde(rename = "match")]
    matcher: CatalogMatch,
    response: CatalogReadResponse,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaRouteFixture {
    #[serde(rename = "match")]
    matcher: MediaMatch,
    response: MediaStreamResponse,
}

/// case 期望：`dto` = 宿主函数写回结构化应答（version/status/error 逐项
/// 断言，`response_fields` 对序列化后的应答做逐字段相等）；`host_error` =
/// 宿主函数级 `Err`（错误文本包含指定片段）。`version`/`status` 写字面值
/// （不从 `CAPABILITY_DTO_VERSION` 生成——常量漂移要被夹具抓住）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ExpectCase {
    Dto(DtoExpect),
    HostError(HostErrorExpect),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DtoExpect {
    /// 线上判别字面值 `"dto"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: DtoKind,
    /// 期望的应答 `version` 字面值。宿主侧依据：入参解析失败与 DTO 版本
    /// 不符一律用宿主常量构造应答；catalog/identity 的权限门与字段拒绝
    /// 用请求 `version`，而宿主 media 分支的错误应答统一走 `media_error`
    /// （恒为宿主常量，不读请求 `version`）。过版本门后请求 `version` 与
    /// 宿主常量必等，两种写法行为无差——夹具统一断言字面值。
    version: u16,
    ok: bool,
    /// 仅 `tma_media_stream` 允许（catalog/identity 应答无 status 字段）。
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    error_code: Option<String>,
    /// 宿主手写文案（确定性）才断言精确 message；解析失败类文案内嵌
    /// serde 错误文本，用 [`Self::error_message_contains`]。
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    error_message_contains: Option<String>,
    /// 对序列化应答逐字段相等断言（透传 case 断言响应逐字段等于注入值）。
    #[serde(default)]
    response_fields: BTreeMap<String, serde_json::Value>,
    dispatch_count: usize,
}

#[derive(Debug, Deserialize)]
enum DtoKind {
    #[serde(rename = "dto")]
    Dto,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostErrorExpect {
    /// 线上判别字面值 `"host_error"`：由 serde 反序列化消费，代码不再读。
    #[allow(dead_code)]
    kind: HostErrorKind,
    #[serde(default)]
    message_contains: Vec<String>,
    dispatch_count: usize,
}

#[derive(Debug, Deserialize)]
enum HostErrorKind {
    #[serde(rename = "host_error")]
    HostError,
}

// ---------------------------------------------------------------------------
// schema 校验（未知字段由 serde 拒；重复 id 与不合法 expect 组合在此大声报）
// ---------------------------------------------------------------------------

impl Case {
    /// 结构性组合校验：期望形态与桩编排必须自洽，夹具写错当场 panic。
    fn validate(&self) {
        let dispatch = match &self.expect {
            ExpectCase::Dto(expect) => {
                assert!(
                    expect.dispatch_count <= 1,
                    "契约 case [{}] dispatch_count 最多为 1（单次调用）",
                    self.id
                );
                if expect.ok {
                    assert!(
                        expect.error_code.is_none(),
                        "契约 case [{}] ok:true 不应断言 error_code",
                        self.id
                    );
                } else {
                    assert!(
                        expect.error_code.is_some(),
                        "契约 case [{}] ok:false 必须断言 error_code",
                        self.id
                    );
                }
                if expect.status.is_some() {
                    assert!(
                        self.function == CapabilityFn::Media,
                        "契约 case [{}] 只有 media 应答有 status 字段",
                        self.id
                    );
                }
                expect.dispatch_count
            }
            ExpectCase::HostError(expect) => {
                assert_eq!(
                    expect.dispatch_count, 1,
                    "契约 case [{}] host_error 必然发生在进桩派发之后",
                    self.id
                );
                assert!(
                    !expect.message_contains.is_empty(),
                    "契约 case [{}] host_error 应断言 message_contains",
                    self.id
                );
                expect.dispatch_count
            }
        };

        let orchestrated = match &self.stub {
            None | Some(StubCase::Strict(_)) => None,
            Some(StubCase::Orchestrated(OrchestratedStub {
                identity,
                catalog_routes,
                media_routes,
                ..
            })) => Some((
                identity.is_some(),
                !catalog_routes.is_empty(),
                !media_routes.is_empty(),
            )),
        };
        let configured = |fn_: CapabilityFn| match orchestrated {
            None => false,
            Some((identity, catalog, media)) => match fn_ {
                CapabilityFn::Catalog => catalog,
                CapabilityFn::Identity => identity,
                CapabilityFn::Media => media,
            },
        };
        match &self.expect {
            ExpectCase::Dto(_) => {
                assert_eq!(
                    configured(self.function),
                    dispatch == 1,
                    "契约 case [{}] dto 期望与桩编排不自洽（dispatch={dispatch}）",
                    self.id
                );
            }
            ExpectCase::HostError(_) => assert!(
                orchestrated.is_none(),
                "契约 case [{}] host_error 应使用缺省/strict_stub 表达未配置",
                self.id
            ),
        }
    }
}

/// 解析 + 校验夹具：未知字段（serde）、重复 id、不合法 expect 组合均大声 panic。
fn load_fixture() -> ContractFixture {
    let fixture: ContractFixture =
        serde_json::from_str(FIXTURE_JSON).expect("契约夹具 capability_host.json 非法");
    assert_eq!(
        fixture.schema_version, 1,
        "契约夹具 schema_version 不受支持"
    );
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    for case in fixture.host_cases.iter().chain(&fixture.testkit_extensions) {
        case.validate();
        assert!(ids.insert(&case.id), "契约夹具 case id 重复: {}", case.id);
    }
    assert!(!fixture.host_cases.is_empty(), "契约夹具 host_cases 为空");
    fixture
}

// ---------------------------------------------------------------------------
// 回放器：每条 case 新建桩与状态，直接打真实 respond 函数
// ---------------------------------------------------------------------------

/// 把 fixture 输入反序列化为请求 DTO：成功/失败形态都原样交给 respond。
/// 与生产共用 [`parse_capability_input`]——`json` 先 `to_vec` 回字节、
/// `raw_json` 直接按 UTF-8 字节，同走真实跨边界路径的 `from_slice`，
/// 消除 `from_value`/`from_slice` 语义差，真实解析路径被夹具覆盖。
fn parse_input<T: serde::de::DeserializeOwned>(input: &InputCase) -> Result<T, String> {
    match input {
        InputCase::Json(JsonInput { value, .. }) => {
            parse_capability_input(&serde_json::to_vec(value).expect("fixture value 应可序列化"))
        }
        InputCase::RawJson(RawJsonInput { value, .. }) => parse_capability_input(value.as_bytes()),
    }
}

/// 回放单条 case：断言失败 panic（case id 打头），否则静默通过。
fn replay_case(case: &Case) {
    let stub = CapabilityStub::new();
    if let Some(StubCase::Orchestrated(OrchestratedStub {
        identity,
        catalog_routes,
        media_routes,
        ..
    })) = &case.stub
    {
        if let Some(response) = identity {
            stub.clone().with_identity(response.clone());
        }
        for route in catalog_routes {
            stub.clone()
                .with_catalog(route.matcher.clone(), route.response.clone());
        }
        for route in media_routes {
            stub.clone()
                .with_media(route.matcher.clone(), route.response.clone());
        }
    }
    let state = HostFnState {
        proxy: StubProxy::new(vec![]),
        capabilities: stub.clone(),
        runtime_config: Arc::from("{}"),
        granted: Some(Arc::new(
            case.granted.iter().cloned().collect::<BTreeSet<String>>(),
        )),
    };

    match case.function {
        CapabilityFn::Catalog => {
            let parsed = parse_input::<CatalogReadRequest>(&case.input);
            match (&case.expect, catalog_respond(&state, parsed)) {
                (ExpectCase::Dto(_), Ok(response)) => assert_dto(case, &response),
                (ExpectCase::HostError(_), Err(err)) => assert_host_error(case, &err),
                (ExpectCase::Dto(_), Err(err)) => panic!(
                    "契约 case [{}] 期望 dto 应答，实际宿主级 Err: {err}",
                    case.id
                ),
                (ExpectCase::HostError(_), Ok(response)) => panic!(
                    "契约 case [{}] 期望宿主级 Err，实际写回 dto 应答: {response:?}",
                    case.id
                ),
            }
            assert_dispatch_count(case, stub.catalog_requests().len());
        }
        CapabilityFn::Identity => {
            let parsed = parse_input::<IdentityReadRequest>(&case.input);
            match (&case.expect, identity_respond(&state, parsed)) {
                (ExpectCase::Dto(_), Ok(response)) => assert_dto(case, &response),
                (ExpectCase::HostError(_), Err(err)) => assert_host_error(case, &err),
                (ExpectCase::Dto(_), Err(err)) => panic!(
                    "契约 case [{}] 期望 dto 应答，实际宿主级 Err: {err}",
                    case.id
                ),
                (ExpectCase::HostError(_), Ok(response)) => panic!(
                    "契约 case [{}] 期望宿主级 Err，实际写回 dto 应答: {response:?}",
                    case.id
                ),
            }
            assert_dispatch_count(case, stub.identity_requests().len());
        }
        CapabilityFn::Media => {
            let parsed = parse_input::<MediaStreamRequest>(&case.input);
            match (&case.expect, media_respond(&state, parsed)) {
                (ExpectCase::Dto(_), Ok(response)) => assert_dto(case, &response),
                (ExpectCase::HostError(_), Err(err)) => assert_host_error(case, &err),
                (ExpectCase::Dto(_), Err(err)) => panic!(
                    "契约 case [{}] 期望 dto 应答，实际宿主级 Err: {err}",
                    case.id
                ),
                (ExpectCase::HostError(_), Ok(response)) => panic!(
                    "契约 case [{}] 期望宿主级 Err，实际写回 dto 应答: {response:?}",
                    case.id
                ),
            }
            assert_dispatch_count(case, stub.media_requests().len());
        }
    }
}

/// host_error 期望断言：错误文本（含 source chain）包含全部指定片段。
fn assert_host_error(case: &Case, err: &extism::Error) {
    let ExpectCase::HostError(expect) = &case.expect else {
        panic!(
            "契约 case [{}] 内部错误：非 host_error 期望进入该断言",
            case.id
        );
    };
    let mut text = err.to_string();
    for cause in err.chain().skip(1) {
        text.push_str(": ");
        text.push_str(&cause.to_string());
    }
    for needle in &expect.message_contains {
        assert!(
            text.contains(needle.as_str()),
            "契约 case [{}] 错误文本应含 {needle:?}: {text:?}",
            case.id
        );
    }
}

/// dto 期望逐项断言：version/status 字面值、ok、error code/message、
/// `response_fields` 逐字段相等（对序列化后的应答 Value 比）。
fn assert_dto<R: serde::Serialize>(case: &Case, response: &R) {
    let ExpectCase::Dto(expect) = &case.expect else {
        panic!("契约 case [{}] 内部错误：非 dto 期望进入 dto 断言", case.id);
    };
    let value = serde_json::to_value(response)
        .unwrap_or_else(|e| panic!("契约 case [{}] 应答序列化失败: {e}", case.id));
    assert_eq!(
        value["version"], expect.version,
        "契约 case [{}] 应答 version 不符: {value}",
        case.id
    );
    assert_eq!(
        value["ok"], expect.ok,
        "契约 case [{}] 应答 ok 不符: {value}",
        case.id
    );
    if let Some(status) = expect.status {
        assert_eq!(
            value["status"], status,
            "契约 case [{}] 应答 status 不符: {value}",
            case.id
        );
    }
    match (&expect.error_code, value.get("error")) {
        (Some(code), Some(error)) => assert_eq!(
            error["code"],
            code.as_str(),
            "契约 case [{}] 应答 error.code 不符: {value}",
            case.id
        ),
        (Some(code), None) => panic!(
            "契约 case [{}] 期望 error.code={code}，应答无 error: {value}",
            case.id
        ),
        (None, Some(error)) => panic!("契约 case [{}] 未期望 error，应答却带: {error}", case.id),
        (None, None) => {}
    }
    if let Some(message) = &expect.error_message {
        assert_eq!(
            value["error"]["message"],
            message.as_str(),
            "契约 case [{}] 应答 error.message 不符",
            case.id
        );
    }
    if let Some(needle) = &expect.error_message_contains {
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(needle),
            "契约 case [{}] 应答 error.message 应含 {needle:?}: {message:?}",
            case.id
        );
    }
    for (field, want) in &expect.response_fields {
        assert_eq!(
            &value[field], want,
            "契约 case [{}] 应答字段 {field} 不符: {}",
            case.id, value[field]
        );
    }
}

/// 派发次数断言：被前置校验/权限门拒绝的调用不留痕。
fn assert_dispatch_count(case: &Case, dispatch_count: usize) {
    let expect = match &case.expect {
        ExpectCase::Dto(expect) => expect.dispatch_count,
        ExpectCase::HostError(expect) => expect.dispatch_count,
    };
    assert_eq!(
        dispatch_count, expect,
        "契约 case [{}] 进桩派发次数不符",
        case.id
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture schema 自检：未知字段/重复 id/不合法组合在加载即大声失败。
    #[test]
    fn contract_fixture_schema_is_valid() {
        let fixture = load_fixture();
        assert!(!fixture.coverage.is_empty(), "coverage 不应为空");
        assert!(!fixture.excluded.is_empty(), "excluded 不应为空");
        assert_eq!(
            fixture.provenance.method, "manual_transcription",
            "夹具是宿主源的手工转写"
        );
        assert_eq!(
            fixture.provenance.capability_dto_version, 1,
            "provenance 记录的 DTO 线格式版本（写字面值，随宿主源同步）"
        );
        assert_eq!(fixture.provenance.repository, "tma");
        assert!(
            fixture.provenance.path.ends_with("capability_fns.rs"),
            "provenance.path 应指向宿主 capability_fns.rs"
        );
        assert_eq!(
            fixture.provenance.commit.len(),
            40,
            "provenance.commit 须为完整 git hash（转写单一事实源的定位）"
        );
        assert!(
            !fixture.provenance.host_sdk_version.is_empty(),
            "provenance 应记录宿主 SDK 版本"
        );
    }

    /// 契约回放：全部 case 逐条打真实 respond 函数。收集全部失败 case 后
    /// 统一报告（一次看到所有红，不逐条 panic 中断）；schema 自检测试仍
    /// 加载即快速失败。
    #[test]
    fn contract_fixture_replays_host_semantics() {
        use std::panic::AssertUnwindSafe;

        let fixture = load_fixture();
        let failures: Vec<String> = fixture
            .host_cases
            .iter()
            .chain(&fixture.testkit_extensions)
            .filter_map(|case| {
                std::panic::catch_unwind(AssertUnwindSafe(|| replay_case(case)))
                    .err()
                    .map(|payload| {
                        let message = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| {
                                payload
                                    .downcast_ref::<&str>()
                                    .map(|text| (*text).to_string())
                            })
                            .unwrap_or_else(|| "未知 panic".to_string());
                        format!("- {message}")
                    })
            })
            .collect();
        assert!(
            failures.is_empty(),
            "契约回放 {} 条 case 失败:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// matcher 未知字段拒绝：路由表里写错的 matcher 字段（如 `codce`）在
    /// fixture 解析期就大声失败，不静默吞成「无约束」放行。
    #[test]
    fn contract_route_matcher_rejects_unknown_fields() {
        let catalog_route = serde_json::json!({
            "match": { "kind": "artist", "codce": "flac" },
            "response": { "version": 1, "ok": true, "items": [] }
        });
        assert!(
            serde_json::from_value::<CatalogRouteFixture>(catalog_route).is_err(),
            "catalog matcher 未知字段必须解析失败"
        );
        let media_route = serde_json::json!({
            "match": { "media_id": "m-1", "media_iid": "m-1" },
            "response": { "version": 1, "ok": true, "status": 200 }
        });
        assert!(
            serde_json::from_value::<MediaRouteFixture>(media_route).is_err(),
            "media matcher 未知字段必须解析失败"
        );
        // 合法字段不受影响。
        assert!(
            serde_json::from_value::<CatalogMatch>(serde_json::json!({ "kind": "artist" })).is_ok()
        );
        assert!(
            serde_json::from_value::<MediaMatch>(serde_json::json!({ "media_id": "m-1" })).is_ok()
        );
    }
}
