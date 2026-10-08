//! subsonic 官方插件验收测试：manifest 契约断言 + 真实签名包行为验收。
//!
//! 行为段走 `testkit::load_dir_with_capabilities` 全链路（wasm 构建 → 测试
//! 私钥 pack → 验签 → manifest 比对 → instance-per-call 实例化），以
//! `call_http` 驱动 `tma_http` 导出，capability 桩应答
//! `tma_catalog_read`/`tma_identity_read`/`tma_media_stream`；
//! protocol/response 的纯逻辑单测随 crate 内 `#[cfg(test)]` 跑。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use testkit::{
    CAPABILITY_DTO_VERSION, CapabilityStub, CatalogMatch, MediaByteRange, MediaMatch,
    PluginErrorCode, PluginHttpRequest, PluginHttpResponse, StubProxy, base64_decode,
    base64_encode, catalog_err, catalog_item, catalog_ok, identity_err, media_err, media_ok,
};

#[derive(Debug, Deserialize)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    abi: Abi,
    extension_points: Vec<String>,
    permissions: Vec<Permission>,
    http: Http,
    author: Option<String>,
    homepage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Abi {
    min: Version,
    max: Version,
}

#[derive(Debug, Deserialize)]
struct Version {
    major: u16,
    minor: u16,
}

#[derive(Debug, Deserialize)]
struct Permission {
    capability: Capability,
}

#[derive(Debug, Deserialize)]
struct Capability {
    name: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct Http {
    routes: Vec<Route>,
}

#[derive(Debug, Deserialize)]
struct Route {
    path: String,
    methods: Vec<String>,
}

#[test]
fn manifest_declares_stable_subsonic_contract() {
    let raw = include_str!("../manifest.json");
    let manifest: Manifest = serde_json::from_str(raw).expect("manifest.json must be valid JSON");
    assert_eq!(manifest.id, "tma.official.subsonic");
    assert_eq!(manifest.name, "Subsonic / OpenSubsonic");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!((manifest.abi.min.major, manifest.abi.min.minor), (1, 6));
    assert_eq!((manifest.abi.max.major, manifest.abi.max.minor), (1, 6));
    assert_eq!(manifest.extension_points, vec!["http"]);
    assert_eq!(
        manifest
            .permissions
            .iter()
            .map(|permission| permission.capability.name.as_str())
            .collect::<Vec<_>>(),
        vec!["catalog.read", "identity.read", "media.stream"]
    );
    assert!(
        manifest.permissions.iter().all(|permission| !permission
            .capability
            .reason
            .trim()
            .is_empty())
    );
    assert_eq!(manifest.http.routes.len(), 1);
    assert_eq!(manifest.http.routes[0].path, "/rest/*");
    assert_eq!(
        manifest.http.routes[0].methods,
        vec!["GET".to_string(), "HEAD".to_string()]
    );
    assert_eq!(manifest.author.as_deref(), Some("TMA"));
    assert!(
        manifest
            .homepage
            .as_deref()
            .is_some_and(|url| url.ends_with("/plugins/subsonic"))
    );
}

#[test]
fn manifest_i18n_covers_en_and_zh_descriptions() {
    let raw = include_str!("../manifest.json");
    let manifest: Value = serde_json::from_str(raw).expect("manifest.json must be valid JSON");
    for locale in ["en", "zh"] {
        let description = manifest["i18n"][locale]["description"].as_str();
        assert!(
            description.is_some_and(|text| !text.trim().is_empty()),
            "i18n.{locale}.description must be present"
        );
    }
    assert_eq!(
        manifest["i18n"]["zh"]["description"].as_str().unwrap(),
        manifest["description"].as_str().unwrap(),
        "zh description 与顶层 description 同源"
    );
}

// ---------------------------------------------------------------------------
// 行为验收：真实签名包 + capability 桩 + tma_http 驱动
// ---------------------------------------------------------------------------

/// 插件实现的 Subsonic 协议版本（`subsonic-response` 的 version 字段）。
const SUBSONIC_VERSION: &str = "1.16.1";

/// 全链路夹具：wasm 构建 → 测试私钥 pack → 验签 → manifest 比对 → 实例化。
/// subsonic 不发 `http_request` 出站调用，stub 代理给空表即可。
fn load(stub: Arc<CapabilityStub>) -> testkit::LoadedPlugin {
    testkit::load_dir_with_capabilities(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        "{}",
        StubProxy::new(vec![]),
        stub,
    )
}

fn get(path: &str, query: &[(&str, &str)]) -> PluginHttpRequest {
    get_with_headers(path, query, BTreeMap::new())
}

/// 构造 `tma_http` 入参：宿主侧已剥离认证参数，`identity` 是路由级身份
/// 快照（本插件实际认 `tma_identity_read` 的应答，不读该字段，置 None）。
fn get_with_headers(
    path: &str,
    query: &[(&str, &str)],
    headers: BTreeMap<String, String>,
) -> PluginHttpRequest {
    PluginHttpRequest {
        method: "GET".into(),
        path: path.into(),
        query: query
            .iter()
            .map(|(name, value)| ((*name).into(), vec![(*value).into()]))
            .collect(),
        headers,
        body_b64: None,
        identity: None,
    }
}

fn body_bytes(resp: &PluginHttpResponse) -> Vec<u8> {
    let b64 = resp.body_b64.as_deref().expect("响应应有 body_b64");
    base64_decode(b64).expect("body_b64 应为合法 base64")
}

fn body_text(resp: &PluginHttpResponse) -> String {
    String::from_utf8(body_bytes(resp)).expect("响应体应为 UTF-8")
}

fn body_json(resp: &PluginHttpResponse) -> Value {
    serde_json::from_slice(&body_bytes(resp)).expect("响应体应为 JSON")
}

/// XML failed 封套断言（业务错误一律 HTTP 200 + `<error code=N>`）。
fn assert_xml_failed(resp: &PluginHttpResponse, code: u16) {
    assert_eq!(resp.status, 200);
    let body = body_text(resp);
    assert!(body.contains("status=\"failed\""), "{body}");
    assert!(body.contains(&format!("code=\"{code}\"")), "{body}");
}

/// 真实签名包链路（`load` 本身即断言 pack/验签/manifest 比对/实例化），
/// 再以 ping 打通 XML 与 f=json 两种封套。ping 不查身份——桩可全未配置。
#[test]
fn signed_package_loads_and_ping_ok_in_both_envelopes() {
    let stub = CapabilityStub::new();
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get(
            "/rest/ping.view",
            &[("v", SUBSONIC_VERSION), ("c", "testkit")],
        ))
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(
        resp.headers["content-type"],
        "application/xml; charset=utf-8"
    );
    let body = body_text(&resp);
    assert!(
        body.contains("<subsonic-response status=\"ok\" version=\"1.16.1\""),
        "{body}"
    );
    assert!(body.contains("<ping version=\"1.16.1\""), "{body}");

    let resp = plugin
        .call_http(get("/rest/ping", &[("v", SUBSONIC_VERSION), ("f", "json")]))
        .unwrap();
    assert_eq!(
        resp.headers["content-type"],
        "application/json; charset=utf-8"
    );
    let envelope = body_json(&resp);
    assert_eq!(envelope["subsonic-response"]["status"], "ok");
    assert_eq!(envelope["subsonic-response"]["version"], "1.16.1");
    assert_eq!(envelope["subsonic-response"]["openSubsonic"], true);

    // ping 不发起任何 capability 宿主调用。
    assert!(stub.identity_requests().is_empty());
    assert!(stub.catalog_requests().is_empty());
    assert!(stub.media_requests().is_empty());
}

/// `tma_identity_read` 应答 ok=false → 已认证端点回 failed 封套 error 50，
/// 且不再发起 catalog/media 调用。
#[test]
fn unauthenticated_identity_fails_50_without_catalog_or_media_calls() {
    let stub =
        CapabilityStub::new().with_identity(identity_err("unauthenticated", "匿名调用无身份"));
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get("/rest/getArtists.view", &[("v", SUBSONIC_VERSION)]))
        .unwrap();
    assert_xml_failed(&resp, 50);
    let resp = plugin
        .call_http(get(
            "/rest/stream.view",
            &[("v", SUBSONIC_VERSION), ("id", "t-1")],
        ))
        .unwrap();
    assert_xml_failed(&resp, 50);

    assert_eq!(stub.identity_requests().len(), 2, "两次调用各查一次身份");
    assert!(stub.catalog_requests().is_empty(), "未认证不碰目录");
    assert!(stub.media_requests().is_empty(), "未认证不碰媒体");
}

/// 协议校验在身份检查之前：缺 `v` → error 10；`v` 超过 1.16.1 或出现未知
/// 参数 → error 0。已认证但未知的端点 → error 0（身份调用照常发生）。
#[test]
fn protocol_validation_rejects_bad_params() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false);
    let plugin = load(stub.clone());

    let resp = plugin.call_http(get("/rest/ping.view", &[])).unwrap();
    assert_xml_failed(&resp, 10);

    let resp = plugin
        .call_http(get("/rest/ping.view", &[("v", "1.17.0")]))
        .unwrap();
    assert_xml_failed(&resp, 0);

    let resp = plugin
        .call_http(get(
            "/rest/ping.view",
            &[("v", SUBSONIC_VERSION), ("bogus", "1")],
        ))
        .unwrap();
    assert_xml_failed(&resp, 0);
    assert!(body_text(&resp).contains("bogus"));

    let resp = plugin
        .call_http(get("/rest/notAnEndpoint.view", &[("v", SUBSONIC_VERSION)]))
        .unwrap();
    assert_xml_failed(&resp, 0);
    assert_eq!(
        stub.identity_requests().len(),
        1,
        "未知端点也先过身份检查再分发"
    );
}

/// 协议校验先于身份检查（ping 之外的端点也成立）：非 ping 端点缺 `v`，
/// 即使身份为 ok=false 也应回 error 10 而非 50，且完全不发起身份调用。
/// 已认证端点的缺失必填参数同样在 catalog 调用前被拦下。
#[test]
fn protocol_validation_precedes_identity_and_dispatch() {
    let stub =
        CapabilityStub::new().with_identity(identity_err("unauthenticated", "匿名调用无身份"));
    let plugin = load(stub.clone());

    let resp = plugin.call_http(get("/rest/getArtists.view", &[])).unwrap();
    assert_xml_failed(&resp, 10);
    assert!(
        stub.identity_requests().is_empty(),
        "缺 v 应在查身份之前被拒"
    );

    let stub = CapabilityStub::authenticated("u-1", "alice", false);
    let plugin = load(stub.clone());
    let resp = plugin
        .call_http(get("/rest/getAlbum.view", &[("v", SUBSONIC_VERSION)]))
        .unwrap();
    assert_xml_failed(&resp, 10);
    assert!(body_text(&resp).contains("id"));
    assert!(stub.catalog_requests().is_empty(), "缺 id 不应发起目录调用");
}

/// capability 调用未配置/未命中时是宿主函数级失败：整个 `tma_http` 调用
/// `Err`（Internal + 错误链带 capability 名），不是 failed 封套——桩缺配置
/// 不会被误读成已配置的业务失败。
#[test]
fn unconfigured_capability_fails_whole_call() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false);
    let plugin = load(stub.clone());

    let err = plugin
        .call_http(get("/rest/getArtists.view", &[("v", SUBSONIC_VERSION)]))
        .unwrap_err();
    assert_eq!(err.code, PluginErrorCode::Internal);
    assert!(
        err.message.contains("tma_catalog_read"),
        "错误链应点名宿主函数: {}",
        err.message
    );
    // 先记录再失败：未命中请求仍留痕。
    assert_eq!(stub.catalog_requests().len(), 1);
    assert_eq!(stub.catalog_requests()[0].kind.as_deref(), Some("artist"));
}

/// `getMusicFolders` → catalog kind=library。
#[test]
fn get_music_folders_reads_library_kind() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false).with_catalog(
        CatalogMatch::kind("library"),
        catalog_ok(vec![catalog_item("lib-1", "library", "Music")]),
    );
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get(
            "/rest/getMusicFolders.view",
            &[("v", SUBSONIC_VERSION)],
        ))
        .unwrap();
    let body = body_text(&resp);
    assert!(body.contains("status=\"ok\""), "{body}");
    assert!(
        body.contains("<musicFolder id=\"lib-1\" name=\"Music\" />"),
        "{body}"
    );

    let reqs = stub.catalog_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].kind.as_deref(), Some("library"));
    assert_eq!(reqs[0].limit, 1000);
    assert_eq!(reqs[0].version, CAPABILITY_DTO_VERSION);
}

/// `getArtists` → catalog kind=artist + name→query / offset→cursor /
/// count→limit 映射；XML 与 f=json 两种封套都验证内容。
#[test]
fn get_artists_maps_pagination_and_renders_both_envelopes() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false).with_catalog(
        CatalogMatch::kind("artist"),
        catalog_ok(vec![
            catalog_item("ar-1", "artist", "Alpha"),
            catalog_item("ar-2", "artist", "Beta & Co"),
        ]),
    );
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get("/rest/getArtists.view", &[("v", SUBSONIC_VERSION)]))
        .unwrap();
    assert_eq!(resp.status, 200);
    let body = body_text(&resp);
    assert!(
        body.contains(
            "<artists><index name=\"\"><artist id=\"ar-1\" name=\"Alpha\" /><artist id=\"ar-2\" name=\"Beta &amp; Co\" /></index></artists>"
        ),
        "{body}"
    );

    let resp = plugin
        .call_http(get(
            "/rest/getArtists.view",
            &[("v", SUBSONIC_VERSION), ("f", "json")],
        ))
        .unwrap();
    let envelope = body_json(&resp);
    let artists = &envelope["subsonic-response"]["artists"]["index"][0]["artist"];
    assert_eq!(artists[0]["id"], "ar-1");
    assert_eq!(artists[1]["name"], "Beta & Co");

    let reqs = stub.catalog_requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].kind.as_deref(), Some("artist"));
    assert_eq!(reqs[0].limit, 20, "count 缺省 20");
    assert!(reqs[0].cursor.is_none(), "offset=0 不出 cursor");
    assert!(reqs[0].id.is_none() && reqs[0].parent_id.is_none());
    assert_eq!(reqs[0].version, CAPABILITY_DTO_VERSION);

    // 分页/过滤参数 → CatalogReadRequest 映射。
    plugin
        .call_http(get(
            "/rest/getArtists.view",
            &[
                ("v", SUBSONIC_VERSION),
                ("count", "5"),
                ("offset", "10"),
                ("name", "Bow"),
            ],
        ))
        .unwrap();
    let reqs = stub.catalog_requests();
    let last = reqs.last().unwrap();
    assert_eq!(last.limit, 5);
    assert_eq!(last.cursor.as_deref(), Some("10"));
    assert_eq!(last.query.as_deref(), Some("Bow"));
}

/// `getAlbum` 是两次 catalog 调用：album by id → song by parent_id；
/// XML 与 f=json 两种封套都验证内容。
#[test]
fn get_album_chains_two_catalog_reads() {
    let mut album = catalog_item("al-1", "album", "Album One");
    album.artist = Some("Alpha".into());
    album.year = Some(1973);
    let mut intro = catalog_item("s-1", "song", "Intro");
    intro.album_id = Some("al-1".into());
    intro.artist = Some("Alpha".into());
    intro.format = Some("flac".into());
    intro.duration_ms = Some(61_000);
    let stub = CapabilityStub::authenticated("u-1", "alice", false)
        .with_catalog(
            CatalogMatch::kind("album").and_id("al-1"),
            catalog_ok(vec![album]),
        )
        .with_catalog(
            CatalogMatch::kind("song").and_parent_id("al-1"),
            catalog_ok(vec![intro, catalog_item("s-2", "song", "Second")]),
        );
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get(
            "/rest/getAlbum.view",
            &[("v", SUBSONIC_VERSION), ("id", "al-1")],
        ))
        .unwrap();
    let body = body_text(&resp);
    assert!(
        body.contains("<album id=\"al-1\" name=\"Album One\" artist=\"Alpha\" songCount=\"2\">"),
        "{body}"
    );
    assert!(
        body.contains("<song id=\"s-1\" title=\"Intro\" isDir=\"false\""),
        "{body}"
    );
    assert!(body.contains("contentType=\"audio/flac\""), "{body}");

    let reqs = stub.catalog_requests();
    assert_eq!(reqs.len(), 2, "album by id → songs by parent_id");
    assert_eq!(reqs[0].kind.as_deref(), Some("album"));
    assert_eq!(reqs[0].id.as_deref(), Some("al-1"));
    assert_eq!(reqs[0].limit, 1);
    assert_eq!(reqs[1].kind.as_deref(), Some("song"));
    assert_eq!(reqs[1].parent_id.as_deref(), Some("al-1"));
    assert_eq!(reqs[1].limit, 1000);

    let resp = plugin
        .call_http(get(
            "/rest/getAlbum.view",
            &[("v", SUBSONIC_VERSION), ("id", "al-1"), ("f", "json")],
        ))
        .unwrap();
    let envelope = body_json(&resp);
    let album = &envelope["subsonic-response"]["album"];
    assert_eq!(album["id"], "al-1");
    assert_eq!(album["songCount"].as_u64(), Some(2));
    assert_eq!(album["song"][0]["id"], "s-1");
    assert_eq!(album["song"][0]["contentType"], "audio/flac");
}

/// catalog 业务失败（ok:false DTO）→ failed 封套 code 0 + 原文 message；
/// 空 items → 实体不存在 → error 70（XML 与 f=json 封套各验一次）。
#[test]
fn catalog_failure_and_missing_entity_map_to_subsonic_errors() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false).with_catalog(
        CatalogMatch::kind("song").and_id("gone"),
        catalog_err("storage", "底层目录读取失败"),
    );
    let plugin = load(stub);
    let resp = plugin
        .call_http(get(
            "/rest/getSong.view",
            &[("v", SUBSONIC_VERSION), ("id", "gone")],
        ))
        .unwrap();
    assert_xml_failed(&resp, 0);
    assert!(body_text(&resp).contains("底层目录读取失败"));

    let stub = CapabilityStub::authenticated("u-1", "alice", false)
        .with_catalog(CatalogMatch::kind("song"), catalog_ok(vec![]));
    let plugin = load(stub);
    let resp = plugin
        .call_http(get(
            "/rest/getSong.view",
            &[("v", SUBSONIC_VERSION), ("id", "missing")],
        ))
        .unwrap();
    assert_xml_failed(&resp, 70);

    let resp = plugin
        .call_http(get(
            "/rest/getSong.view",
            &[("v", SUBSONIC_VERSION), ("id", "missing"), ("f", "json")],
        ))
        .unwrap();
    let envelope = body_json(&resp);
    assert_eq!(envelope["subsonic-response"]["status"], "failed");
    assert_eq!(
        envelope["subsonic-response"]["error"]["code"].as_u64(),
        Some(70)
    );
}

/// `stream`：Range 头 + format/maxBitRate → `MediaStreamRequest` 映射；
/// 宿主 206 + content-range + stream_id 原样透传为响应。
#[test]
fn stream_maps_media_request_and_forwards_response() {
    let mut media = media_ok(206, "audio/flac", Some(base64_encode(b"DATA")));
    media.content_length = Some(90);
    media.content_range = Some("bytes 10-99/1000".into());
    media.stream_id = Some("st-1".into());
    let stub = CapabilityStub::authenticated("u-1", "alice", false)
        .with_media(MediaMatch::media_id("track-1"), media);
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get_with_headers(
            "/rest/stream.view",
            &[
                ("v", SUBSONIC_VERSION),
                ("id", "track-1"),
                ("format", "flac"),
                ("maxBitRate", "320"),
            ],
            BTreeMap::from([("range".into(), "bytes=10-99".into())]),
        ))
        .unwrap();
    assert_eq!(resp.status, 206);
    assert_eq!(resp.headers["content-type"], "audio/flac");
    assert_eq!(resp.headers["content-range"], "bytes 10-99/1000");
    assert_eq!(resp.headers["content-length"], "90");
    assert_eq!(resp.headers["x-tma-stream-id"], "st-1");
    assert_eq!(body_bytes(&resp), b"DATA");

    let reqs = stub.media_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].version, CAPABILITY_DTO_VERSION);
    assert_eq!(reqs[0].media_id, "track-1");
    assert_eq!(reqs[0].codec.as_deref(), Some("flac"));
    assert_eq!(reqs[0].bitrate_kbps, Some(320));
    assert_eq!(
        reqs[0].range,
        Some(MediaByteRange {
            start: 10,
            end: Some(99)
        })
    );
}

/// `stream`：宿主 ok=false + status 404 → Subsonic error 70。
#[test]
fn stream_media_failure_maps_to_error_70() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false).with_media(
        MediaMatch::media_id("missing"),
        media_err(404, "not_found", "曲目不存在"),
    );
    let plugin = load(stub);

    let resp = plugin
        .call_http(get(
            "/rest/stream.view",
            &[("v", SUBSONIC_VERSION), ("id", "missing")],
        ))
        .unwrap();
    assert_xml_failed(&resp, 70);
    assert!(body_text(&resp).contains("曲目不存在"));
}

/// `getCoverArt`：裸 id 补 `cover:` 前缀，已带 `cover:`/`track:`/`artist:`
/// 前缀的 media_id 原样透传；content-type 透传宿主响应。
#[test]
fn cover_art_prefixes_media_id_and_passes_content_type() {
    let stub = CapabilityStub::authenticated("u-1", "alice", false).with_media(
        MediaMatch::default(),
        media_ok(200, "image/jpeg", Some(base64_encode(b"IMG"))),
    );
    let plugin = load(stub.clone());

    let resp = plugin
        .call_http(get(
            "/rest/getCoverArt.view",
            &[("v", SUBSONIC_VERSION), ("id", "abc")],
        ))
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.headers["content-type"], "image/jpeg");

    for id in ["cover:x", "track:9", "artist:7"] {
        plugin
            .call_http(get(
                "/rest/getCoverArt.view",
                &[("v", SUBSONIC_VERSION), ("id", id)],
            ))
            .unwrap();
    }

    let ids: Vec<String> = stub
        .media_requests()
        .into_iter()
        .map(|req| req.media_id)
        .collect();
    assert_eq!(ids, ["cover:abc", "cover:x", "track:9", "artist:7"]);
}
