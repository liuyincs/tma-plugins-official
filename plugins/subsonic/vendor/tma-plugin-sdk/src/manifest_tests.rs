//! Tests for the plugin manifest contract.

use super::*;
use crate::abi::{AbiVersion, HOST_ABI, HTTP_MIN_ABI, PLAYLIST_IMPORT_MIN_ABI};
use crate::events::PluginEventKind;
use crate::http_route::HttpRouteMethod;
use crate::permission::{HttpScheme, Permission, PermissionError};
use crate::scrape::ScrapeCapability;

/// 契约夹具：`fixtures/manifest.minimal.json`（Wikidata 形态，
/// 仅必填字段——无凭据第一档来源的最小清单）。
const MINIMAL_FIXTURE: &str = include_str!("../fixtures/manifest.minimal.json");

/// 契约夹具：`fixtures/manifest.full.json`（Last.fm 形态，
/// 全字段——凭据来源 + 简介/作者/主页齐备）。
const FULL_FIXTURE: &str = include_str!("../fixtures/manifest.full.json");

/// 契约夹具：`fixtures/manifest.http.json`（ABI 1.6 入站 HTTP
/// 与宿主 capability 权限形态）。
const HTTP_FIXTURE: &str = include_str!("../fixtures/manifest.http.json");

/// golden 守卫：夹具 → 结构体 → JSON 必须与夹具原文值等价，
/// 夹具漂移（字段改名/形态变化）在此处第一时间爆出。
fn assert_golden(fixture: &'static str) -> PluginManifest {
    let manifest: PluginManifest = serde_json::from_str(fixture).expect("夹具必须是合法清单");
    let expected: serde_json::Value = serde_json::from_str(fixture).expect("夹具必须是合法 JSON");
    let actual = serde_json::to_value(&manifest).unwrap();
    assert_eq!(actual, expected, "清单序列化形态与夹具漂移");
    manifest
}

#[test]
fn minimal_fixture_golden() {
    let m = assert_golden(MINIMAL_FIXTURE);
    assert_eq!(m.id, "tma.builtin.wikidata");
    let scrape = m.scrape.as_ref().unwrap();
    assert_eq!(scrape.provider, "wikidata");
    assert_eq!(scrape.capabilities, vec![ScrapeCapability::ExternalIds]);
    assert!(!scrape.requires_credentials);
    assert_eq!(m.description, None);
    assert_eq!(m.author, None);
    assert_eq!(m.homepage, None);
    // 旧形态（无 permissions 键）仍可解析：空权限 = 无副作用能力。
    assert!(m.permissions.is_empty());
    assert!(m.has_extension_point("scrape_provider"));
    m.validate().unwrap();
    assert!(m.abi_compatible(HOST_ABI));
}

#[test]
fn full_fixture_golden() {
    let m = assert_golden(FULL_FIXTURE);
    assert_eq!(m.id, "tma.official.lastfm");
    let scrape = m.scrape.as_ref().unwrap();
    assert_eq!(scrape.provider, "last_fm");
    assert_eq!(
        scrape.capabilities,
        vec![
            ScrapeCapability::ExternalIds,
            ScrapeCapability::Bio,
            ScrapeCapability::Image,
        ]
    );
    assert!(scrape.requires_credentials);
    assert!(m.description.is_some());
    assert!(m.author.is_some());
    assert!(m.homepage.is_some());
    // later ABI 管理面：配置表单 schema 与敏感属性名。
    let schema = m.config_schema.as_ref().unwrap();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["language"]["enum"].is_array());
    assert_eq!(m.config_secrets, Some(vec!["api_key".to_string()]));
    // ABI 1.1 夹具：出站 HTTP 权限 + ABI 1.4 夹具：单轮 AI 权限（顺序保持声明序）。
    assert_eq!(
        m.permissions,
        vec![
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "ws.audioscrobbler.com".into(),
                reason: "Last.fm 艺术家简介/图片接口".into(),
                traffic: None,
            },
            Permission::Ai {
                reason: "用单轮 AI 从自由文本抽取曲目列表".into(),
            }
        ]
    );
    // ABI 1.2 夹具：actions + i18n（含 config/actions 的 $ 引用样例）。
    assert_eq!(
        m.actions,
        Some(vec![ActionManifest {
            id: "refresh_bio_cache".into(),
            label: "$actions.refresh_bio_cache.label".into(),
        }])
    );
    // ABI 1.4 夹具：playlist_import 扩展点（method label 与 input_schema 的
    // title 都走 $ 引用，i18n.en 齐备）。
    let import = m.playlist_import.as_ref().unwrap();
    assert_eq!(import.methods.len(), 1);
    assert_eq!(import.methods[0].id, "from_text");
    assert_eq!(
        import.methods[0].label,
        "$playlist_import.from_text.label".to_string()
    );
    assert!(import.methods[0].requires_ai);
    let input_schema = import.methods[0].input_schema.as_ref().unwrap();
    assert_eq!(input_schema["required"], serde_json::json!(["text"]));
    // 厂商扩展键 `x-ui` 原样保留（结构校验不拒绝未知键）。
    assert_eq!(input_schema["properties"]["text"]["x-ui"], "textarea");
    let i18n = m.i18n.as_ref().unwrap();
    assert!(i18n.contains_key("en") && i18n.contains_key("zh"));
    assert_eq!(i18n["en"]["config.api_key.title"], "Last.fm API key");
    assert_eq!(
        i18n["en"]["playlist_import.from_text.label"],
        "Import from pasted text"
    );
    // 顶层 name/description 语义不变（= 默认语言 en 文案）。
    assert_eq!(m.name, "Last.fm");
    assert_eq!(
        m.description.as_deref(),
        Some("Last.fm 艺术家简介与图片（需 api_key）")
    );
    m.validate().unwrap();
    assert!(m.abi_compatible(HOST_ABI));
    assert!(!m.abi_compatible(AbiVersion::new(2, 0)));
}

#[test]
fn http_fixture_golden_and_capability_permissions() {
    let m = assert_golden(HTTP_FIXTURE);
    assert_eq!(m.id, "tma.official.subsonic");
    assert!(m.abi_compatible(HOST_ABI));
    assert_eq!(m.abi.min, HTTP_MIN_ABI);
    assert!(m.has_extension_point("http"));

    assert_eq!(m.permissions.len(), 3);
    let capability_names: Vec<_> = m
        .permissions
        .iter()
        .filter_map(|permission| match permission {
            Permission::Capability { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        capability_names,
        vec!["catalog.read", "identity.read", "media.stream"]
    );

    let http = m.http.as_ref().expect("http 扩展点必须有路由段");
    assert_eq!(http.routes.len(), 3);
    assert_eq!(http.routes[0].path, "/rest/ping");
    assert_eq!(http.routes[0].methods, vec![HttpRouteMethod::Get]);
    assert_eq!(
        http.routes[2].methods,
        vec![HttpRouteMethod::Get, HttpRouteMethod::Head]
    );
    m.validate().unwrap();
}

#[test]
fn previous_minor_manifest_remains_compatible_with_host() {
    let mut manifest: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    manifest.abi.max = AbiVersion::new(1, 5);
    manifest.validate().unwrap();
    assert!(manifest.abi_compatible(HOST_ABI));
}

#[test]
fn http_extension_requires_abi_1_6_and_routes() {
    let mut m: PluginManifest = serde_json::from_str(HTTP_FIXTURE).unwrap();

    m.abi.min = AbiVersion::new(1, 5);
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));

    m.abi.min = HTTP_MIN_ABI;
    m.http.as_mut().unwrap().routes[0].path = "/rest//ping".into();
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));

    m.http.as_mut().unwrap().routes[0].path = " /rest/ping".into();
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));
    m.http.as_mut().unwrap().routes[0].path = "/rest/ping ".into();
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));
    m.http.as_mut().unwrap().routes[0].path = "/rest/*/ping".into();
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));
    m.http.as_mut().unwrap().routes[0].path = "/rest/ping*".into();
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));
    m.http.as_mut().unwrap().routes[0].path = "/rest/*".into();
    assert!(m.validate().is_ok());

    m.http.as_mut().unwrap().routes[0].path = "/rest/ping".into();
    m.http.as_mut().unwrap().routes[0]
        .methods
        .push(HttpRouteMethod::Get);
    assert!(matches!(m.validate(), Err(ManifestError::InvalidHttp(_))));

    m.http = None;
    assert_eq!(m.validate(), Err(ManifestError::MissingHttpConfig));
}

#[test]
fn validate_rejects_structural_errors() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();

    let empty_id = PluginManifest {
        id: "  ".into(),
        ..base.clone()
    };
    assert_eq!(empty_id.validate(), Err(ManifestError::EmptyId));

    let bad_abi = PluginManifest {
        abi: AbiRange {
            min: AbiVersion::new(1, 2),
            max: AbiVersion::new(1, 0),
        },
        ..base.clone()
    };
    assert!(matches!(
        bad_abi.validate(),
        Err(ManifestError::EmptyAbiRange { .. })
    ));

    let no_scrape = PluginManifest {
        scrape: None,
        ..base
    };
    assert_eq!(
        no_scrape.validate(),
        Err(ManifestError::MissingScrapeConfig)
    );
}

/// id 字符集（`^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*$`，1..=200 字符）的合法侧。
#[test]
fn validate_accepts_legal_id_charset() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    // 多段反域名 / 单段 / 下划线与数字混合 / 恰好 200 字符上限。
    let max_len = "a".repeat(200);
    for id in [
        "tma.builtin.musicbrainz".to_string(),
        "solo".to_string(),
        "x_9.Y_0".to_string(),
        max_len,
    ] {
        let m = PluginManifest { id, ..base.clone() };
        m.validate()
            .unwrap_or_else(|e| panic!("id {:?} 应合法：{e:?}", m.id));
    }
}

#[test]
fn builtin_plugin_id_prefix_is_reserved() {
    assert!(super::is_builtin_plugin_id("tma.builtin.musicbrainz"));
    assert!(!super::is_builtin_plugin_id("com.example.musicbrainz"));
    assert!(!super::is_builtin_plugin_id("tma.builtins.trap"));
}

/// id 字符集非法侧：`-`（前缀清理歧义正主）、空串、`/`、空格、非 ASCII、
/// 前导/尾随/连续点、超长，各拒绝一例。
#[test]
fn validate_rejects_illegal_id_charset() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();

    // 空串/纯空白仍走既有 EmptyId 变体（语义不变）。
    for id in ["", "  "] {
        let m = PluginManifest {
            id: id.into(),
            ..base.clone()
        };
        assert_eq!(m.validate(), Err(ManifestError::EmptyId));
    }

    let illegal: Vec<String> = [
        "tma.example.http-echo".to_string(), // `-`：`{id}-` 前缀清理会误删他插件
        "com.my-site.x".to_string(),
        "tma.evil/x".to_string(),    // `/`：防路径语义
        "tma.plugin v2".to_string(), // 空格
        "tma.插件".to_string(),      // 非 ASCII
        ".tma.plugin".to_string(),   // 前导点
        "tma.plugin.".to_string(),   // 尾随点
        "tma..plugin".to_string(),   // 连续点
        "a".repeat(201),             // 超长
    ]
    .to_vec();
    for id in illegal {
        let m = PluginManifest { id, ..base.clone() };
        assert!(
            matches!(m.validate(), Err(ManifestError::InvalidId(_))),
            "id {:?} 应被拒绝",
            m.id
        );
    }
}

#[test]
fn validate_rejects_bad_permissions_with_index() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let bad = PluginManifest {
        permissions: vec![
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "a.com".into(),
                reason: "ok".into(),
                traffic: None,
            },
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "  ".into(),
                reason: "x".into(),
                traffic: None,
            },
        ],
        ..base
    };
    match bad.validate() {
        Err(ManifestError::InvalidPermissionAt { index, source }) => {
            assert_eq!(index, 1);
            assert_eq!(source, PermissionError::EmptyHost);
        }
        other => panic!("期望 InvalidPermissionAt，实际 {other:?}"),
    }
}

/// permissions 往返：结构体 → JSON → 结构体逐字段一致。
#[test]
fn permissions_roundtrip() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let m = PluginManifest {
        permissions: vec![
            Permission::Http {
                scheme: HttpScheme::Https,
                host: "*.example.com".into(),
                reason: "多套图 CDN".into(),
                traffic: None,
            },
            Permission::Http {
                scheme: HttpScheme::Http,
                host: "127.0.0.1:8080".into(),
                reason: "本地回环测试".into(),
                traffic: None,
            },
        ],
        ..base
    };
    m.validate().unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

/// ABI 1.0 时代的清单（无 permissions 键）在 1.1 宿主上仍可解析与校验。
#[test]
fn legacy_manifest_without_permissions_parses() {
    let json = r#"{
            "id": "tma.legacy.plugin",
            "name": "Legacy",
            "version": "0.0.1",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 1 } },
            "extension_points": ["scrape_provider"],
            "scrape": {
                "provider": "wikidata",
                "capabilities": ["external_ids"],
                "requires_credentials": false
            }
        }"#;
    let m: PluginManifest = serde_json::from_str(json).unwrap();
    assert!(m.permissions.is_empty());
    m.validate().unwrap();
    // 序列化时省略空 permissions（线形态与旧契约一致）。
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("permissions"));
}

#[test]
fn unknown_extension_point_deserializes_without_crashing() {
    let json = r#"{
            "id": "tma.test.plugin",
            "name": "Test",
            "version": "0.1.0",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 6 } },
            "extension_points": ["future_point", "scrape_provider"],
            "scrape": {
                "provider": "wikidata",
                "capabilities": ["external_ids"],
                "requires_credentials": false
            }
        }"#;
    let m: PluginManifest = serde_json::from_str(json).unwrap();
    assert!(m.has_extension_point("future_point"));
    assert!(m.has_extension_point("scrape_provider"));
    m.validate().unwrap();
    assert!(m.abi_compatible(HOST_ABI));
}

#[test]
fn manifest_serde_roundtrip() {
    let m: PluginManifest = serde_json::from_str(FULL_FIXTURE).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

/// 带 config_schema/config_secrets 的清单可直接通过校验。
#[test]
fn config_schema_subset_parses_and_validates() {
    let m: PluginManifest = serde_json::from_str(FULL_FIXTURE).unwrap();
    m.validate().unwrap();
}

#[test]
fn config_schema_rejects_non_object_and_array_types() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();

    let not_object = PluginManifest {
        config_schema: Some(serde_json::json!(["string"])),
        ..base.clone()
    };
    assert!(matches!(
        not_object.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));

    let array_prop = PluginManifest {
        config_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {"tags": {"type": "array"}}
        })),
        ..base.clone()
    };
    assert!(matches!(
        array_prop.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));

    let top_type_string = PluginManifest {
        config_schema: Some(serde_json::json!({"type": "string"})),
        ..base
    };
    assert!(matches!(
        top_type_string.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));
}

#[test]
fn config_schema_rejects_combinators_and_untyped_properties() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();

    let one_of = PluginManifest {
        config_schema: Some(serde_json::json!({
            "properties": {"mode": {"oneOf": [{"type": "string"}, {"type": "number"}]}}
        })),
        ..base.clone()
    };
    assert!(matches!(
        one_of.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));

    let untyped = PluginManifest {
        config_schema: Some(serde_json::json!({"properties": {"mode": {"title": "模式"}}})),
        ..base.clone()
    };
    assert!(matches!(
        untyped.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));
}

/// 嵌套 object 分组递归同规则；const 枚举形态合法。
#[test]
fn config_schema_allows_nested_groups_and_const() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let m = PluginManifest {
        config_schema: Some(serde_json::json!({
            "type": "object",
            "required": ["mode"],
            "properties": {
                "mode": {"const": "fast"},
                "http": {
                    "type": "object",
                    "properties": {"timeout_seconds": {"type": "number"}}
                },
                "nested_bad": {
                    "type": "object",
                    "properties": {"list": {"type": "array"}}
                }
            }
        })),
        ..base
    };
    // 嵌套分组内的数组属性同样被拒。
    assert!(matches!(
        m.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));
}

#[test]
fn config_values_reject_read_only_fields_at_all_nesting_levels() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "session_key": {"type": "string", "readOnly": true},
            "auth": {"type": "object", "properties": {
                "token": {"type": "string", "readOnly": true}
            }}
        }
    });
    let top = validate_config_values(Some(&schema), &serde_json::json!({"session_key": "x"}))
        .unwrap_err();
    assert!(top.to_string().contains("session_key"));
    let nested =
        validate_config_values(Some(&schema), &serde_json::json!({"auth": {"token": "x"}}))
            .unwrap_err();
    assert!(nested.to_string().contains("auth.token"));
}

#[test]
fn config_secrets_empty_name_is_rejected() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let m = PluginManifest {
        config_secrets: Some(vec!["  ".to_string()]),
        ..base
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::InvalidConfigSecrets(_))
    ));
}

/// 凭据门槛的前置约束：requires_credentials=true 必须声明非空 config_secrets
///（None 与空数组同属作者错误）；false 则无此要求。
#[test]
fn requires_credentials_without_config_secrets_is_rejected() {
    let full: PluginManifest = serde_json::from_str(FULL_FIXTURE).unwrap();
    assert!(full.scrape.as_ref().unwrap().requires_credentials);

    let missing = PluginManifest {
        config_secrets: None,
        ..full.clone()
    };
    assert!(matches!(
        missing.validate(),
        Err(ManifestError::InvalidConfigSecrets(_))
    ));

    let empty = PluginManifest {
        config_secrets: Some(vec![]),
        ..full
    };
    assert!(matches!(
        empty.validate(),
        Err(ManifestError::InvalidConfigSecrets(_))
    ));

    // requires_credentials=false 的清单不声明 config_secrets 仍合法。
    let minimal: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert!(minimal.config_secrets.is_none());
    minimal.validate().unwrap();
}

/// 旧清单（无 config_* 键）解析为 None，序列化省略（线形态不变）。
#[test]
fn legacy_manifest_without_config_fields_stays_none() {
    let m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert!(m.config_schema.is_none());
    assert!(m.config_secrets.is_none());
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("config_schema"));
    assert!(!out.contains("config_secrets"));
}

/// 旧清单（无 i18n/actions 键）解析为 None，序列化省略（线形态不变）。
#[test]
fn legacy_manifest_without_actions_i18n_stays_none() {
    let m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert!(m.i18n.is_none());
    assert!(m.actions.is_none());
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("i18n"));
    assert!(!out.contains("actions"));
    // 无 $ 引用时缺 i18n 表也合法（零迁移：字面文案不依赖 i18n）。
    m.validate().unwrap();
}

/// actions/i18n 往返：结构体 → JSON → 结构体逐字段一致（含 $ 引用原样保留）。
#[test]
fn actions_and_i18n_roundtrip() {
    let m: PluginManifest = serde_json::from_str(FULL_FIXTURE).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

/// 声明非空 actions 的清单直接通过校验（abi.min ≥ 1.2 由 full 夹具保证）。
#[test]
fn actions_with_sufficient_abi_validate() {
    let m: PluginManifest = serde_json::from_str(FULL_FIXTURE).unwrap();
    assert!(m.abi.min >= crate::abi::ACTIONS_MIN_ABI);
    m.validate().unwrap();
}

/// locale 键形态：合法侧（宽松口径，实际使用 zh/en）。
#[test]
fn i18n_accepts_legal_locale_keys() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let locales: Vec<String> = ["zh", "en", "pt", "zh-Hans", "zh-Hans-CN", "en-US", "es-419"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut m = base.clone();
    m.i18n = Some(
        locales
            .iter()
            .map(|l| (l.clone(), BTreeMap::from([("name".into(), "x".into())])))
            .collect(),
    );
    m.validate()
        .unwrap_or_else(|e| panic!("locales {locales:?} 应全部合法：{e:?}"));
}

/// locale 键形态非法侧：大写主段、单字母、数字开头、下划线分隔、非 ASCII、
/// 单字符后续段、空段，各拒绝一例。
#[test]
fn i18n_rejects_illegal_locale_keys() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    for locale in [
        "ZH", "z", "zhz z", "1zh", "zh_CN", "中文", "zh-Hans-", "zh-x",
    ] {
        let mut m = base.clone();
        m.i18n = Some(BTreeMap::from([(
            locale.to_string(),
            BTreeMap::from([("name".into(), "x".into())]),
        )]));
        assert!(
            matches!(m.validate(), Err(ManifestError::InvalidI18n(_))),
            "locale {locale:?} 应被拒绝"
        );
    }
}

/// i18n 文案键为空字符串 → 拒绝（类型已保证值形态，键只挡空串）。
#[test]
fn i18n_rejects_empty_text_key() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let mut m = base;
    m.i18n = Some(BTreeMap::from([(
        "en".to_string(),
        BTreeMap::from([("".to_string(), "x".to_string())]),
    )]));
    assert!(matches!(m.validate(), Err(ManifestError::InvalidI18n(_))));
}

/// action id 字符集合法侧（小写开头、下划线/数字、1..=64 字符）。
#[test]
fn actions_accept_legal_id_charset() {
    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi.min = crate::abi::ACTIONS_MIN_ABI;
    for id in [
        "a".to_string(),
        "refresh_cache".into(),
        "x9_y0".into(),
        "a".repeat(64),
    ] {
        let m = PluginManifest {
            actions: Some(vec![ActionManifest {
                id,
                label: "刷新".into(),
            }]),
            ..base.clone()
        };
        m.validate()
            .unwrap_or_else(|e| panic!("id {:?} 应合法：{e:?}", m.actions));
    }
}

/// action id 非法侧：大写、连字符、数字开头、非 ASCII、空串、超 64 字符、重复。
#[test]
fn actions_reject_illegal_ids_and_duplicates() {
    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi.min = crate::abi::ACTIONS_MIN_ABI;
    for id in [
        "Refresh".to_string(),
        "refresh-cache".into(),
        "1refresh".into(),
        "刷新".into(),
        String::new(),
        "a".repeat(65),
    ] {
        let m = PluginManifest {
            actions: Some(vec![ActionManifest {
                id: id.clone(),
                label: "刷新".into(),
            }]),
            ..base.clone()
        };
        assert!(
            matches!(m.validate(), Err(ManifestError::InvalidActions(_))),
            "id {id:?} 应被拒绝",
        );
    }
    let duplicate = PluginManifest {
        actions: Some(vec![
            ActionManifest {
                id: "refresh".into(),
                label: "刷新".into(),
            },
            ActionManifest {
                id: "refresh".into(),
                label: "再刷新".into(),
            },
        ]),
        ..base
    };
    assert!(matches!(
        duplicate.validate(),
        Err(ManifestError::InvalidActions(_))
    ));
}

/// ABI 门槛：非空 actions 且 abi.min < 1.2 → 拒绝；空列表不受门槛约束。
#[test]
fn actions_require_abi_min_1_2() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert_eq!(base.abi.min, AbiVersion::new(1, 0));

    let declared = PluginManifest {
        actions: Some(vec![ActionManifest {
            id: "refresh".into(),
            label: "刷新".into(),
        }]),
        ..base.clone()
    };
    assert!(matches!(
        declared.validate(),
        Err(ManifestError::InvalidActions(_))
    ));

    // 恰好 1.2：放行。
    let bumped = PluginManifest {
        abi: AbiRange {
            min: AbiVersion::new(1, 2),
            max: base.abi.max,
        },
        ..declared
    };
    bumped.validate().unwrap();

    // 空列表 = 未声明动作，abi.min 1.0 也放行。
    let empty = PluginManifest {
        actions: Some(vec![]),
        ..base
    };
    empty.validate().unwrap();
}

/// `$` 引用悬空拒绝：缺 en 表 / 缺被引用键；字面文案（无 $）零迁移。
#[test]
fn dollar_refs_require_resolvable_en_table() {
    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi.min = crate::abi::ACTIONS_MIN_ABI;
    base.config_schema = Some(serde_json::json!({
        "properties": {
            "api_key": {"type": "string", "title": "$config.api_key.title"},
            "http": {
                "type": "object",
                "properties": {"timeout_seconds": {"type": "number", "description": "$config.http.timeout_seconds.description"}}
            }
        }
    }));
    base.actions = Some(vec![ActionManifest {
        id: "refresh".into(),
        label: "$actions.refresh.label".into(),
    }]);

    // 无 i18n：存在 $ 引用但缺 en 表 → 拒绝。
    assert!(matches!(
        base.validate(),
        Err(ManifestError::DanglingI18nRef(_))
    ));

    // 只有 zh 表（无 en）：同样拒绝。
    let zh_only = PluginManifest {
        i18n: Some(BTreeMap::from([(
            "zh".into(),
            BTreeMap::from([
                ("config.api_key.title".into(), "API 密钥".into()),
                (
                    "config.http.timeout_seconds.description".into(),
                    "超时".into(),
                ),
                ("actions.refresh.label".into(), "刷新".into()),
            ]),
        )])),
        ..base.clone()
    };
    assert!(matches!(
        zh_only.validate(),
        Err(ManifestError::DanglingI18nRef(_))
    ));

    // en 表缺一个键（action label 悬空）：拒绝，错误带位置。
    let missing_key = PluginManifest {
        i18n: Some(BTreeMap::from([(
            "en".into(),
            BTreeMap::from([
                ("config.api_key.title".into(), "API key".into()),
                (
                    "config.http.timeout_seconds.description".into(),
                    "timeout".into(),
                ),
            ]),
        )])),
        ..base.clone()
    };
    match missing_key.validate() {
        Err(ManifestError::DanglingI18nRef(msg)) => {
            assert!(msg.contains("actions.refresh.label"), "{msg}");
        }
        other => panic!("期望 DanglingI18nRef，实际 {other:?}"),
    }

    // en 表含全部被引用键（含嵌套属性与 action label）：放行。
    let complete = PluginManifest {
        i18n: Some(BTreeMap::from([(
            "en".into(),
            BTreeMap::from([
                ("config.api_key.title".into(), "API key".into()),
                (
                    "config.http.timeout_seconds.description".into(),
                    "timeout".into(),
                ),
                ("actions.refresh.label".into(), "Refresh".into()),
            ]),
        )])),
        ..base.clone()
    };
    complete.validate().unwrap();

    // 字面文案（不以 $ 开头）不构成引用：无 i18n 也放行（存量插件零迁移）。
    let literal = PluginManifest {
        config_schema: Some(serde_json::json!({
            "properties": {"api_key": {"type": "string", "title": "API key"}}
        })),
        actions: Some(vec![ActionManifest {
            id: "refresh".into(),
            label: "刷新".into(),
        }]),
        ..base
    };
    literal.validate().unwrap();
}

// ==================== ABI 1.3：scrobble_reporter / 每用户配置 ====================
/// scrobble_reporter 测试基线：abi.min 1.3 的最小清单（无刮削配置的独立插件）。
fn reporter_base() -> PluginManifest {
    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi = AbiRange {
        min: crate::abi::EVENTS_MIN_ABI,
        max: HOST_ABI,
    };
    base.extension_points = vec!["scrobble_reporter".to_string()];
    base.scrape = None;
    base.scrobble_reporter = Some(ScrobbleReporterManifest {
        events: vec![PluginEventKind::Scrobble, PluginEventKind::NowPlaying],
        library_types: None,
    });
    base
}

/// 合法声明：两种事件全订阅 + 每用户配置齐备，直接通过校验。
#[test]
fn scrobble_reporter_and_user_config_validate() {
    let mut m = reporter_base();
    m.user_config_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {
            "session_key": {"type": "string", "title": "Session key"},
            "scrobble_percent": {"type": "number"}
        },
        "required": ["session_key"]
    }));
    m.user_config_secrets = vec!["session_key".to_string()];
    m.validate().unwrap();

    // 只订阅一种事件同样合法（宿主只推送已订阅种类）。
    let single = PluginManifest {
        scrobble_reporter: Some(ScrobbleReporterManifest {
            events: vec![PluginEventKind::Scrobble],
            library_types: None,
        }),
        ..reporter_base()
    };
    single.validate().unwrap();
}

/// 扩展点声明但缺嵌套块 → 拒绝（对齐 scrape 块先例的存在性校验）。
#[test]
fn scrobble_reporter_requires_nested_block() {
    let missing = PluginManifest {
        scrobble_reporter: None,
        ..reporter_base()
    };
    assert_eq!(
        missing.validate(),
        Err(ManifestError::MissingScrobbleReporterConfig)
    );
}

/// events 为空 → 拒绝；非法事件名在解析层拒绝（类型化枚举，对齐 capabilities 先例）。
#[test]
fn scrobble_reporter_rejects_empty_and_unknown_events() {
    let empty = PluginManifest {
        scrobble_reporter: Some(ScrobbleReporterManifest {
            events: vec![],
            library_types: None,
        }),
        ..reporter_base()
    };
    assert!(matches!(
        empty.validate(),
        Err(ManifestError::InvalidScrobbleReporter(_))
    ));

    // 未知事件名（如未来种类/拼写错误）不是校验错误而是解析错误：
    // events 的取值闭集由 Vec<PluginEventKind> 的 serde 形态保证。
    let json = r#"{
        "id": "tma.test.reporter",
        "name": "R",
        "version": "0.1.0",
        "abi": { "min": { "major": 1, "minor": 3 }, "max": { "major": 1, "minor": 3 } },
        "extension_points": ["scrobble_reporter"],
        "scrobble_reporter": {"events": ["listen"]}
    }"#;
    assert!(serde_json::from_str::<PluginManifest>(json).is_err());
}

/// ABI 门槛：声明 scrobble_reporter / user_config_* 任一而 abi.min < 1.3 → 拒绝。
#[test]
fn abi_1_3_fields_require_events_min_abi() {
    let old_range = AbiRange {
        min: AbiVersion::new(1, 2),
        max: HOST_ABI,
    };

    let reporter = PluginManifest {
        abi: old_range,
        ..reporter_base()
    };
    assert!(matches!(
        reporter.validate(),
        Err(ManifestError::InvalidScrobbleReporter(_))
    ));

    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi = old_range;
    let with_schema = PluginManifest {
        user_config_schema: Some(serde_json::json!({
            "properties": {"session_key": {"type": "string"}}
        })),
        ..base.clone()
    };
    assert!(matches!(
        with_schema.validate(),
        Err(ManifestError::InvalidUserConfig(_))
    ));

    let with_secrets = PluginManifest {
        user_config_secrets: vec!["session_key".to_string()],
        ..base
    };
    assert!(matches!(
        with_secrets.validate(),
        Err(ManifestError::InvalidUserConfig(_))
    ));

    // 门槛恰好 1.3：三者齐备放行。
    let bumped = PluginManifest {
        abi: AbiRange {
            min: crate::abi::EVENTS_MIN_ABI,
            max: HOST_ABI,
        },
        ..with_secrets
    };
    bumped.validate().unwrap();
}

/// 每用户配置校验口径与全局配置同构：受限子集、空 secrets 条目拒绝。
#[test]
fn user_config_schema_follows_config_schema_subset_rules() {
    let mut base = reporter_base();

    base.user_config_schema = Some(serde_json::json!(["string"]));
    assert!(matches!(
        base.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));

    base.user_config_schema = Some(serde_json::json!({
        "properties": {"tags": {"type": "array"}}
    }));
    assert!(matches!(
        base.validate(),
        Err(ManifestError::InvalidConfigSchema(_))
    ));

    base.user_config_schema = None;
    base.user_config_secrets = vec!["  ".to_string()];
    assert!(matches!(
        base.validate(),
        Err(ManifestError::InvalidUserConfig(_))
    ));
}

/// user_config_schema 的 `$` 引用与 config_schema 同口径：须能被 i18n.en 解析。
#[test]
fn user_config_schema_dollar_refs_require_en_table() {
    let mut m = reporter_base();
    m.user_config_schema = Some(serde_json::json!({
        "properties": {"session_key": {"type": "string", "title": "$user_config.session_key.title"}}
    }));
    // 存在 $ 引用但缺 i18n.en 表 → 拒绝；补齐 en 表后放行。
    assert!(matches!(
        m.validate(),
        Err(ManifestError::DanglingI18nRef(_))
    ));
    m.i18n = Some(BTreeMap::from([(
        "en".to_string(),
        BTreeMap::from([(
            "user_config.session_key.title".to_string(),
            "Session key".into(),
        )]),
    )]));
    m.validate().unwrap();
}

/// 旧 manifest（无任何 1.3 新字段）照常解析与校验；序列化省略新键（线形态不变）。
#[test]
fn legacy_manifest_without_event_fields_stays_default() {
    let m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert!(m.user_config_schema.is_none());
    assert!(m.user_config_secrets.is_empty());
    assert!(m.scrobble_reporter.is_none());
    assert!(!m.has_extension_point("scrobble_reporter"));
    m.validate().unwrap();
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("user_config_schema"));
    assert!(!out.contains("user_config_secrets"));
    assert!(!out.contains("scrobble_reporter"));
}

/// scrobble_reporter + 每用户配置完整往返：结构体 → JSON → 结构体逐字段一致。
#[test]
fn scrobble_reporter_and_user_config_roundtrip() {
    let m = reporter_base();
    let json = serde_json::to_string(&m).unwrap();
    let back: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

// ==================== ABI 1.4：playlist_import 扩展点 ====================

/// playlist_import 测试基线：abi.min 1.4 的最小清单（无刮削配置的独立插件）。
fn import_base() -> PluginManifest {
    let mut base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    base.abi = AbiRange {
        min: PLAYLIST_IMPORT_MIN_ABI,
        max: HOST_ABI,
    };
    base.extension_points = vec!["playlist_import".to_string()];
    base.scrape = None;
    base.playlist_import = Some(PlaylistImportManifest {
        methods: vec![PlaylistImportMethod {
            id: "from_text".into(),
            label: "从文本导入".into(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {"text": {"type": "string", "title": "曲目列表文本"}}
            })),
            requires_ai: true,
        }],
    });
    base
}

/// 合法声明：非空 methods + input_schema + requires_ai，直接通过校验。
#[test]
fn playlist_import_section_validates() {
    import_base().validate().unwrap();

    // input_schema 缺省 = 方法不接受任何输入，同样合法。
    let no_schema = PluginManifest {
        playlist_import: Some(PlaylistImportManifest {
            methods: vec![PlaylistImportMethod {
                id: "from_clipboard".into(),
                label: "从剪贴板导入".into(),
                input_schema: None,
                requires_ai: false,
            }],
        }),
        ..import_base()
    };
    no_schema.validate().unwrap();

    // requires_ai=true 的方法不必强制声明 Permission::Ai（能力与权限分层：
    // 闸门侧的"缺权限即拒绝"由宿主实施，清单校验不越权代替）。
    let without_permission = PluginManifest {
        permissions: vec![],
        ..import_base()
    };
    without_permission.validate().unwrap();
}

/// 扩展点声明但缺嵌套块 → 拒绝（对齐 scrape / scrobble_reporter 的存在性校验）。
#[test]
fn playlist_import_requires_nested_block() {
    let missing = PluginManifest {
        playlist_import: None,
        ..import_base()
    };
    assert_eq!(
        missing.validate(),
        Err(ManifestError::MissingPlaylistImportConfig)
    );
}

/// methods 为空 → 拒绝；method id 字符集/重复 → 拒绝。
#[test]
fn playlist_import_rejects_empty_methods_and_bad_ids() {
    let empty = PluginManifest {
        playlist_import: Some(PlaylistImportManifest { methods: vec![] }),
        ..import_base()
    };
    assert!(matches!(
        empty.validate(),
        Err(ManifestError::InvalidPlaylistImport(_))
    ));

    for id in [
        "From_Text".to_string(), // 大写开头
        "from-text".into(),      // 连字符
        "1from".into(),          // 数字开头
        "从文本".into(),         // 非 ASCII
        String::new(),           // 空串
        "a".repeat(65),          // 超 64 字符
    ] {
        let m = PluginManifest {
            playlist_import: Some(PlaylistImportManifest {
                methods: vec![PlaylistImportMethod {
                    id: id.clone(),
                    label: "导入".into(),
                    input_schema: None,
                    requires_ai: false,
                }],
            }),
            ..import_base()
        };
        assert!(
            matches!(m.validate(), Err(ManifestError::InvalidPlaylistImport(_))),
            "id {id:?} 应被拒绝",
        );
    }

    let duplicate = PluginManifest {
        playlist_import: Some(PlaylistImportManifest {
            methods: vec![
                PlaylistImportMethod {
                    id: "from_text".into(),
                    label: "从文本导入".into(),
                    input_schema: None,
                    requires_ai: false,
                },
                PlaylistImportMethod {
                    id: "from_text".into(),
                    label: "再导一次".into(),
                    input_schema: None,
                    requires_ai: true,
                },
            ],
        }),
        ..import_base()
    };
    assert!(matches!(
        duplicate.validate(),
        Err(ManifestError::InvalidPlaylistImport(_))
    ));
}

/// ABI 门槛：声明 playlist_import 而 abi.min < 1.4 → 拒绝；恰好 1.4 放行。
#[test]
fn playlist_import_requires_abi_min_1_4() {
    let old = PluginManifest {
        abi: AbiRange {
            min: AbiVersion::new(1, 3),
            max: HOST_ABI,
        },
        ..import_base()
    };
    assert!(matches!(
        old.validate(),
        Err(ManifestError::InvalidPlaylistImport(_))
    ));

    // 只有扩展点名、无嵌套块时 ABI 门槛在存在性校验之后：缺块仍是缺块错误。
    let bare = PluginManifest {
        abi: AbiRange {
            min: AbiVersion::new(1, 0),
            max: HOST_ABI,
        },
        playlist_import: None,
        ..import_base()
    };
    assert_eq!(
        bare.validate(),
        Err(ManifestError::MissingPlaylistImportConfig)
    );
}

/// input_schema 与 config_schema 同一受限子集：数组/组合子/缺 type 一律拒绝。
#[test]
fn playlist_import_input_schema_follows_config_subset_rules() {
    for schema in [
        serde_json::json!(["string"]),
        serde_json::json!({"properties": {"text": {"type": "array"}}}),
        serde_json::json!({"properties": {"mode": {"oneOf": [{"type": "string"}]}}}),
        serde_json::json!({"properties": {"text": {"title": "无类型"}}}),
        serde_json::json!({"type": "string"}),
    ] {
        let mut base = import_base();
        if let Some(section) = base.playlist_import.as_mut() {
            section.methods[0].input_schema = Some(schema.clone());
        }
        assert!(
            matches!(base.validate(), Err(ManifestError::InvalidConfigSchema(_))),
            "schema {schema} 应被拒绝"
        );
    }
}

/// 锁死前向兼容：属性 object 内的未知键（厂商扩展/UI hint）不拒绝，
/// 取值层对 string 值照常放行。未来收紧时此测试必须先显式改语义。
#[test]
fn input_schema_allows_vendor_extension_keys() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["text"],
        "properties": {
            "text": {
                "type": "string",
                "title": "曲目列表文本",
                "x-ui": "textarea",
                "x-tma-rows": 8,
                "x-vendor-hint": {"placeholder": "每行一首"}
            }
        }
    });
    let mut base = import_base();
    if let Some(section) = base.playlist_import.as_mut() {
        section.methods[0].input_schema = Some(schema.clone());
    }
    base.validate().unwrap();

    // 同一 schema 在 config_schema 上的行为一致（受限子集是同一套规则）。
    let mut global = import_base();
    global.config_schema = Some(schema.clone());
    global.validate().unwrap();

    // 取值层：string 值照常通过，未知 hint 键不参与校验。
    validate_config_values(Some(&schema), &serde_json::json!({"text": "1. Song"})).unwrap();
    // readOnly/enum/const 等既有规则不受未知键影响。
    let readonly = serde_json::json!({
        "properties": {"text": {"type": "string", "x-ui": "textarea", "readOnly": true}}
    });
    assert!(matches!(
        validate_config_values(Some(&readonly), &serde_json::json!({"text": "x"})),
        Err(ConfigValuesError::Invalid(_))
    ));
}

/// method label 的 `$` 引用与 config/actions 同口径：须能被 i18n.en 解析。
#[test]
fn playlist_import_label_dollar_refs_require_en_table() {
    let mut m = import_base();
    if let Some(section) = m.playlist_import.as_mut() {
        section.methods[0].label = "$playlist_import.from_text.label".into();
        if let Some(schema) = section.methods[0].input_schema.as_mut() {
            *schema = serde_json::json!({
                "properties": {"text": {"type": "string", "title": "$playlist_import.from_text.text.title"}}
            });
        }
    }
    // 存在 $ 引用但缺 i18n.en 表 → 拒绝。
    assert!(matches!(
        m.validate(),
        Err(ManifestError::DanglingI18nRef(_))
    ));

    // en 表缺 method label 键：拒绝，错误带位置（playlist_import.<id>.label）。
    let partial = PluginManifest {
        i18n: Some(BTreeMap::from([(
            "en".into(),
            BTreeMap::from([(
                "playlist_import.from_text.text.title".into(),
                "Track list text".into(),
            )]),
        )])),
        ..m.clone()
    };
    match partial.validate() {
        Err(ManifestError::DanglingI18nRef(msg)) => {
            assert!(msg.contains("playlist_import.from_text.label"), "{msg}");
        }
        other => panic!("期望 DanglingI18nRef，实际 {other:?}"),
    }

    // en 表齐备：放行。
    let complete = PluginManifest {
        i18n: Some(BTreeMap::from([(
            "en".into(),
            BTreeMap::from([
                (
                    "playlist_import.from_text.label".into(),
                    "Import from pasted text".into(),
                ),
                (
                    "playlist_import.from_text.text.title".into(),
                    "Track list text".into(),
                ),
            ]),
        )])),
        ..m
    };
    complete.validate().unwrap();
}

/// 旧 manifest（无 playlist_import 键）照常解析与校验；序列化省略该键（线形态不变）。
#[test]
fn legacy_manifest_without_playlist_import_stays_default() {
    let m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    assert!(m.playlist_import.is_none());
    assert!(!m.has_extension_point("playlist_import"));
    m.validate().unwrap();
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("playlist_import"));
}

/// playlist_import 段完整往返：结构体 → JSON → 结构体逐字段一致（含自由 JSON 的
/// input_schema 与 `x-ui` 扩展键原样保留）。
#[test]
fn playlist_import_roundtrip() {
    let mut m = import_base();
    if let Some(section) = m.playlist_import.as_mut()
        && let Some(schema) = section.methods[0].input_schema.as_mut()
    {
        schema["properties"]["text"]["x-ui"] = serde_json::json!("textarea");
    }
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("input_schema"), "input_schema 不应被省略");
    assert!(json.contains(r#""x-ui":"textarea""#), "扩展键应原样保留");
    let back: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

// ==================== ABI 1.5：library_types ====================

use crate::library_type::LibraryType;

/// 缺省：未声明 `library_types` 时解析为 `["music"]`，且序列化省略该键
/// （1.4 及更早清单零变化、可继续加载）。
#[test]
fn library_types_default_to_music_when_absent() {
    let m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    let scrape = m.scrape.as_ref().unwrap();
    assert_eq!(scrape.library_types, None);
    assert_eq!(scrape.resolved_library_types(), &[LibraryType::Music]);
    assert!(m.scrobble_reporter.is_none());
    m.validate().unwrap();
    let out = serde_json::to_string(&m).unwrap();
    assert!(!out.contains("library_types"), "缺省不落线：{out}");
}

/// 显式声明（abi.min ≥ 1.5）合法；序列化保留该键。
#[test]
fn library_types_explicit_valid() {
    let mut m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    m.abi = AbiRange {
        min: crate::abi::LIBRARY_TYPES_MIN_ABI,
        max: HOST_ABI,
    };
    m.scrape.as_mut().unwrap().library_types = Some(vec![
        LibraryType::Music,
        LibraryType::Audiobook,
        LibraryType::Podcast,
    ]);
    m.validate().unwrap();
    assert_eq!(
        m.scrape.as_ref().unwrap().resolved_library_types(),
        &[
            LibraryType::Music,
            LibraryType::Audiobook,
            LibraryType::Podcast
        ][..]
    );
    let out = serde_json::to_string(&m).unwrap();
    assert!(out.contains(r#""library_types":["music","audiobook","podcast"]"#));
}

/// 空数组 / 重复项 / abi.min < 1.5 → 拒绝。
#[test]
fn library_types_invalid_declarations_are_rejected() {
    let base: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();

    let empty = {
        let mut m = base.clone();
        m.abi = AbiRange {
            min: crate::abi::LIBRARY_TYPES_MIN_ABI,
            max: HOST_ABI,
        };
        m.scrape.as_mut().unwrap().library_types = Some(vec![]);
        m
    };
    assert!(matches!(
        empty.validate(),
        Err(ManifestError::InvalidLibraryTypes(_))
    ));

    let duplicate = {
        let mut m = base.clone();
        m.abi = AbiRange {
            min: crate::abi::LIBRARY_TYPES_MIN_ABI,
            max: HOST_ABI,
        };
        m.scrape.as_mut().unwrap().library_types =
            Some(vec![LibraryType::Music, LibraryType::Music]);
        m
    };
    assert!(matches!(
        duplicate.validate(),
        Err(ManifestError::InvalidLibraryTypes(_))
    ));

    // 显式字段配旧区间（1.4）→ 作者错误。
    let stale_abi = {
        let mut m = base.clone();
        m.abi = AbiRange {
            min: AbiVersion::new(1, 4),
            max: HOST_ABI,
        };
        m.scrape.as_mut().unwrap().library_types = Some(vec![LibraryType::Music]);
        m
    };
    assert!(matches!(
        stale_abi.validate(),
        Err(ManifestError::InvalidLibraryTypes(_))
    ));
}

/// 非法取值在解析层拒绝（结构无法构造）。
#[test]
fn library_types_unknown_value_fails_parse() {
    let json = r#"{
        "id": "tma.test.scraper",
        "name": "S",
        "version": "0.1.0",
        "abi": { "min": { "major": 1, "minor": 5 }, "max": { "major": 1, "minor": 5 } },
        "extension_points": ["scrape_provider"],
        "scrape": {
            "provider": "s",
            "capabilities": ["external_ids"],
            "requires_credentials": false,
            "library_types": ["video"]
        }
    }"#;
    assert!(serde_json::from_str::<PluginManifest>(json).is_err());
}

/// 1.4 清单（无该字段）继续加载：缺省 music。
#[test]
fn abi_1_4_manifest_without_library_types_loads() {
    let json = r#"{
        "id": "tma.test.scraper",
        "name": "S",
        "version": "0.1.0",
        "abi": { "min": { "major": 1, "minor": 4 }, "max": { "major": 1, "minor": 4 } },
        "extension_points": ["scrape_provider"],
        "scrape": {
            "provider": "s",
            "capabilities": ["external_ids"],
            "requires_credentials": false
        }
    }"#;
    let m: PluginManifest = serde_json::from_str(json).unwrap();
    m.validate().unwrap();
    assert_eq!(
        m.scrape.as_ref().unwrap().resolved_library_types(),
        &[LibraryType::Music][..]
    );
}

/// 上报段同样支持显式 `library_types`（abi.min ≥ 1.5），空数组拒绝。
#[test]
fn scrobble_reporter_library_types() {
    let mut m: PluginManifest = serde_json::from_str(MINIMAL_FIXTURE).unwrap();
    m.abi = AbiRange {
        min: crate::abi::LIBRARY_TYPES_MIN_ABI,
        max: HOST_ABI,
    };
    m.extension_points = vec!["scrobble_reporter".to_string()];
    m.scrape = None;
    m.scrobble_reporter = Some(ScrobbleReporterManifest {
        events: vec![PluginEventKind::Scrobble],
        library_types: Some(vec![LibraryType::Music]),
    });
    m.validate().unwrap();
    assert_eq!(
        m.scrobble_reporter
            .as_ref()
            .unwrap()
            .resolved_library_types(),
        &[LibraryType::Music][..]
    );

    if let Some(reporter) = m.scrobble_reporter.as_mut() {
        reporter.library_types = Some(vec![]);
    }
    assert!(matches!(
        m.validate(),
        Err(ManifestError::InvalidLibraryTypes(_))
    ));
}
