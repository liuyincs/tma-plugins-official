//! subsonic 官方插件验收测试：manifest 契约断言。
//!
//! testkit 目前只桩 `http_request`/`tma_config` 宿主函数并驱动
//! `scrape`/`tma_action`/`tma_event` 导出，不覆盖 `tma_http` 与
//! `tma_catalog_read`/`tma_identity_read`/`tma_media_stream`，故本插件暂不做
//! wasm 行为级断言；protocol/response 的纯逻辑单测随 crate 内 `#[cfg(test)]` 跑。

use serde::Deserialize;
use serde_json::Value;

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
