//! 插件清单 schema：宿主加载插件前的第一道判定依据。
//!
//! golden 夹具在 `fixtures/`（最小/完整/HTTP 三例），本模块测试
//! 以 `include_str!` 编译期嵌入同源消费：
//! 夹具 JSON 既是 schema 文档，也是 Rust 侧反序列化的守卫。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::abi::{
    ACTIONS_MIN_ABI, AbiRange, EVENTS_MIN_ABI, HTTP_MIN_ABI, LIBRARY_TYPES_MIN_ABI,
    PLAYLIST_IMPORT_MIN_ABI,
};
use crate::events::PluginEventKind;
use crate::http_route::HttpManifest;
use crate::library_type::{LibraryType, resolve_library_types};
use crate::permission::Permission;
use crate::scrape::ScrapeCapability;

/// 官方内置插件 id 前缀。仅播种路径可占用；手动安装/升级必须拒绝。
pub const BUILTIN_PLUGIN_ID_PREFIX: &str = "tma.builtin.";

/// 清单 id 是否占用官方内置前缀。
pub fn is_builtin_plugin_id(id: &str) -> bool {
    id.starts_with(BUILTIN_PLUGIN_ID_PREFIX)
}

/// 刮削扩展点清单段（`extension_points` 含 `scrape_provider` 时必填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrapeManifest {
    /// 来源 slug（如 `wikidata`）；与落库 `entity_external_ids.provider` 对齐。
    pub provider: String,
    /// 声明的能力列表。
    pub capabilities: Vec<ScrapeCapability>,
    /// 是否要求宿主注入凭据（缺凭据则不注册）。
    pub requires_credentials: bool,
    /// 支持刮削的资料库类型（ABI 1.5 起）。`None` = 缺省 `["music"]`；
    /// 显式声明时须非空、无重复，且 `abi.min` ≥ 1.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_types: Option<Vec<LibraryType>>,
}

impl ScrapeManifest {
    /// 解析后的支持类型：未声明 = `["music"]`。
    pub fn resolved_library_types(&self) -> &[LibraryType] {
        resolve_library_types(self.library_types.as_deref())
    }
}

/// scrobble 上报扩展点清单段（`extension_points` 含 `scrobble_reporter` 时必填，
/// ABI 1.3 起）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrobbleReporterManifest {
    /// 订阅的事件种类列表（合法值 = [`PluginEventKind`] wire 名：`scrobble` /
    /// `now_playing`；非空——声明扩展点却不订阅任何事件属作者错误）。
    /// 宿主只推送已订阅种类，插件应导出 `tma_event` 接收
    /// [`crate::events::PluginEventRequest`]。
    pub events: Vec<PluginEventKind>,
    /// 支持上报的资料库类型（ABI 1.5 起）。`None` = 缺省 `["music"]`；
    /// 显式声明时须非空、无重复，且 `abi.min` ≥ 1.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_types: Option<Vec<LibraryType>>,
}

impl ScrobbleReporterManifest {
    /// 解析后的支持类型：未声明 = `["music"]`。
    pub fn resolved_library_types(&self) -> &[LibraryType] {
        resolve_library_types(self.library_types.as_deref())
    }
}

/// 插件动作清单段（ABI 1.2 起，`actions` 可选）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionManifest {
    /// 动作 id（`^[a-z][a-z0-9_]{0,63}$`，列表内唯一；宿主按它路由到 `tma_action`）。
    pub id: String,
    /// 展示文案：字面文本或 `$` 前缀 i18n 引用（约定键 `actions.<id>.label`）。
    pub label: String,
}

/// 歌单导入扩展点清单段（`extension_points` 含 `playlist_import` 时必填，
/// ABI 1.4 起，见 [`PLAYLIST_IMPORT_MIN_ABI`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistImportManifest {
    /// 导入方法列表（非空——声明扩展点却不提供任何方法属作者错误）。
    /// 宿主按 method id 路由到插件的 `tma_playlist_import` 导出。
    pub methods: Vec<PlaylistImportMethod>,
}

/// 单个歌单导入方法。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistImportMethod {
    /// 方法 id（与 action id 同一字符集 `^[a-z][a-z0-9_]{0,63}$`，列表内唯一；
    /// 会进 URL 路径与前端路由，字符集从严）。
    pub id: String,
    /// 展示文案：字面文本或 `$` 前缀 i18n 引用（约定键
    /// `playlist_import.<id>.label`，与 `actions.<id>.label` 同构）。
    pub label: String,
    /// 方法输入的 JSON Schema（与 `config_schema` **同一套**受限子集：
    /// [`validate_config_schema`]/[`validate_config_property`]；属性 object 内
    /// 允许 `x-ui: "textarea"` 这类厂商扩展键，结构校验不拒绝未知键）。
    /// 缺省 = 方法不接受任何输入（宿主传空 object）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// 是否需要调用宿主单轮 AI（`ai_chat`）。`true` 时清单应同时声明
    /// [`Permission::Ai`]（宿主闸门据此放行，见 [`crate::ai_host`]）。
    pub requires_ai: bool,
}

/// 插件清单。顶层仅身份与 ABI；刮削配置嵌套在 `scrape`。
///
/// `Eq` 不可派生：`config_schema` 是自由 JSON（`serde_json::Value` 只支持 `PartialEq`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件唯一 id（反向前缀，如 `tma.builtin.musicbrainz`；字符集约束见
    /// [`PluginManifest::validate`]）。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 插件自身版本，必须是规范 SemVer。
    pub version: String,
    /// 兼容的宿主 ABI 闭区间（[`crate::abi`]）。
    pub abi: AbiRange,
    /// 接入的扩展点列表（未知值前向兼容：反序列化保留，严格校验在 [`validate`]）。
    pub extension_points: Vec<String>,
    /// 声明的权限（ABI 1.1 起；空 = 无任何能力，宿主默认拒绝一切副作用）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
    /// 刮削配置（`extension_points` 含 `scrape_provider` 时必填）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrape: Option<ScrapeManifest>,
    /// 一句话用途说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 作者/维护者。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// 项目主页或文档。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// 管理面配置表单（JSON Schema 受限子集，结构校验见 [`validate`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// 敏感配置属性名（值经 AES-256-GCM 加密落库，接口不回显）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_secrets: Option<Vec<String>>,
    /// 每用户配置表单（ABI 1.3 起）：结构与校验同 [`PluginManifest::config_schema`]
    /// （同一 JSON Schema 受限子集），值按 `(plugin_id, user_id)` 维度存取。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_config_schema: Option<serde_json::Value>,
    /// 每用户敏感配置属性名（ABI 1.3 起；语义同 [`PluginManifest::config_secrets`]，
    /// 白名单作用于 user_config_schema 声明的表单）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_config_secrets: Vec<String>,
    /// scrobble 上报配置（`extension_points` 含 `scrobble_reporter` 时必填，
    /// ABI 1.3 起）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrobble_reporter: Option<ScrobbleReporterManifest>,
    /// 歌单导入配置（`extension_points` 含 `playlist_import` 时必填，ABI 1.4 起）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_import: Option<PlaylistImportManifest>,
    /// 通用入站 HTTP 路由（ABI 1.6 起，`tma_http` 导出）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpManifest>,
    /// 国际化文案表（ABI 1.2 起）：`{ "<locale>": { "<键>": "<文本>" } }`。
    ///
    /// 保留键 `name`/`description` 覆盖顶层默认语言（en）文案；其余键为点分
    /// 路径字符串，供 `$` 键引用解析（见 [`PluginManifest::validate`] 的
    /// 悬空引用校验；实际解析在前端，服务端只做存在性校验）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i18n: Option<BTreeMap<String, BTreeMap<String, String>>>,
    /// 插件动作声明（ABI 1.2 起；非空时 `abi.min` 须 ≥ 1.2，且插件应导出
    /// `tma_action` 接收 [`crate::message::PluginActionRequest`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<ActionManifest>>,
}

impl PluginManifest {
    /// 宿主侧 ABI 判定：本清单是否可在指定宿主 ABI 上加载。
    pub fn abi_compatible(&self, host: crate::abi::AbiVersion) -> bool {
        if self.abi.contains(host) {
            return true;
        }
        // minor ABI 只增加可选 DTO。保留上一 minor 插件在下一 minor 宿主上的
        // 兼容性；声明新扩展点的清单仍由各自的 ABI 门槛拒绝旧区间。
        self.abi.min.major == host.major
            && self.abi.max.major == host.major
            && self.abi.max.minor.saturating_add(1) == host.minor
            && self.http.is_none()
    }

    /// 是否声明接入指定扩展点。
    pub fn has_extension_point(&self, point: &str) -> bool {
        self.extension_points.iter().any(|p| p == point)
    }

    /// 结构校验：serde 之外的语义约束（空 id、id 字符集、空 ABI 区间、扩展点缺配套
    /// 字段、i18n 表形态、actions/playlist_import/http 的 id/路由与 ABI 门槛、`$` 引用可被
    /// i18n.en 解析）。
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.trim().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        if !is_valid_id(&self.id) {
            return Err(ManifestError::InvalidId(format!(
                "{:?} 不合法：id 仅允许字母/数字/下划线按点分段（如 tma.builtin.musicbrainz），\
                 整体 1..=200 字符；禁止 `-`、空格、`/` 及空段（前导/尾随/连续点）",
                self.id
            )));
        }
        if !self.abi.is_valid() {
            return Err(ManifestError::EmptyAbiRange {
                min: self.abi.min,
                max: self.abi.max,
            });
        }
        crate::version::parse_version(&self.version)
            .map_err(|e| ManifestError::InvalidVersion(e.to_string()))?;
        if self.has_extension_point("scrape_provider") {
            match &self.scrape {
                None => return Err(ManifestError::MissingScrapeConfig),
                Some(s) if s.provider.trim().is_empty() => {
                    return Err(ManifestError::MissingScrapeConfig);
                }
                Some(s) => {
                    validate_library_types(s.library_types.as_deref(), self.abi.min, "scrape")?
                }
            }
        }
        if self.has_extension_point("scrobble_reporter") {
            match &self.scrobble_reporter {
                None => return Err(ManifestError::MissingScrobbleReporterConfig),
                Some(reporter) => validate_scrobble_reporter(reporter, self.abi.min)?,
            }
        }
        if self.has_extension_point("playlist_import") {
            match &self.playlist_import {
                None => return Err(ManifestError::MissingPlaylistImportConfig),
                Some(section) => validate_playlist_import(section, self.abi.min)?,
            }
        }
        if self.has_extension_point("http") {
            match &self.http {
                None => return Err(ManifestError::MissingHttpConfig),
                Some(section) => validate_http_manifest(section, self.abi.min)?,
            }
        } else if self.http.is_some() {
            return Err(ManifestError::InvalidHttp(
                "声明 http 配置时必须同时声明 http 扩展点".into(),
            ));
        }
        for (index, permission) in self.permissions.iter().enumerate() {
            if let Err(e) = permission.validate() {
                return Err(ManifestError::InvalidPermissionAt { index, source: e });
            }
            if matches!(permission, Permission::Capability { .. }) && self.abi.min < HTTP_MIN_ABI {
                return Err(ManifestError::InvalidHttp(format!(
                    "声明 capability 权限要求 abi.min >= 1.6（当前 {:?}）",
                    self.abi.min
                )));
            }
        }
        if let Some(schema) = &self.config_schema {
            validate_config_schema(schema, "config_schema")?;
        }
        if let Some(secrets) = &self.config_secrets
            && secrets.iter().any(|name| name.trim().is_empty())
        {
            return Err(ManifestError::InvalidConfigSecrets(
                "config_secrets 条目不得为空字符串".into(),
            ));
        }
        // 每用户配置（ABI 1.3 起）：结构子集/空条目校验同全局配置，
        // 声明任一字段即要求 abi.min ≥ 1.3（新字段配旧区间的作者错误）。
        if let Some(schema) = &self.user_config_schema {
            validate_config_schema(schema, "user_config_schema")?;
        }
        if self
            .user_config_secrets
            .iter()
            .any(|name| name.trim().is_empty())
        {
            return Err(ManifestError::InvalidUserConfig(
                "user_config_secrets 条目不得为空字符串".into(),
            ));
        }
        if (self.user_config_schema.is_some() || !self.user_config_secrets.is_empty())
            && self.abi.min < EVENTS_MIN_ABI
        {
            return Err(ManifestError::InvalidUserConfig(format!(
                "声明 user_config_schema/user_config_secrets 要求 abi.min >= 1.3（当前 {:?}）",
                self.abi.min
            )));
        }
        // 凭据门槛（宿主侧按 config_secrets 声明的点分路径判定「已配置」）要求
        // requires_credentials=true 的清单必须声明非空 config_secrets；
        // 未声明属作者错误，在清单校验层直接拒绝，门槛逻辑方可无条件依赖它。
        if self.scrape.as_ref().is_some_and(|s| s.requires_credentials)
            && self.config_secrets.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(ManifestError::InvalidConfigSecrets(
                "requires_credentials=true 的清单必须声明非空 config_secrets \
                 （宿主按声明的点分路径判定凭据是否已配置）"
                    .into(),
            ));
        }
        if let Some(tables) = &self.i18n {
            validate_i18n_tables(tables)?;
        }
        if let Some(actions) = &self.actions {
            validate_actions(actions, self.abi.min)?;
        }
        validate_i18n_refs(self)?;
        Ok(())
    }
}

/// `i18n` 表校验：locale 键形态 + 文案键非空（值为扁平 string→string 已由类型保证）。
fn validate_i18n_tables(
    tables: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), ManifestError> {
    for locale in tables.keys() {
        if !is_valid_locale(locale) {
            return Err(ManifestError::InvalidI18n(format!(
                "locale 键 {locale:?} 不合法（主段 2-3 个小写字母，后续 `-` 段 2-8 个\
                 字母/数字，如 zh / en-US / zh-Hans）"
            )));
        }
    }
    for table in tables.values() {
        if table.keys().any(|k| k.is_empty()) {
            return Err(ManifestError::InvalidI18n(
                "i18n 文案键不得为空字符串".into(),
            ));
        }
    }
    Ok(())
}

/// `scrobble_reporter` 校验：events 非空 + 声明订阅要求 `abi.min` ≥ 1.3。
///
/// events 的取值闭集（`scrobble`/`now_playing`）由 serde 类型
/// （`Vec<PluginEventKind>`）在解析层保证——与 `scrape.capabilities` 的
/// `Vec<ScrapeCapability>` 先例同构。
fn validate_scrobble_reporter(
    reporter: &ScrobbleReporterManifest,
    abi_min: crate::abi::AbiVersion,
) -> Result<(), ManifestError> {
    if reporter.events.is_empty() {
        return Err(ManifestError::InvalidScrobbleReporter(
            "events 不得为空（声明 scrobble_reporter 却不订阅任何事件属作者错误）".into(),
        ));
    }
    validate_library_types(
        reporter.library_types.as_deref(),
        abi_min,
        "scrobble_reporter",
    )?;
    if abi_min < EVENTS_MIN_ABI {
        return Err(ManifestError::InvalidScrobbleReporter(format!(
            "声明 scrobble_reporter 要求 abi.min >= 1.3（当前 {abi_min:?}）"
        )));
    }
    Ok(())
}

/// `library_types` 校验：显式声明须非空、无重复，且要求 `abi.min` ≥ 1.5。
///
/// 取值闭集由 serde 类型（`Vec<LibraryType>`）在解析层保证；缺省（`None`）不校验。
/// `section` 仅用于错误文案（`scrape` / `scrobble_reporter`）。
fn validate_library_types(
    declared: Option<&[LibraryType]>,
    abi_min: crate::abi::AbiVersion,
    section: &str,
) -> Result<(), ManifestError> {
    let Some(types) = declared else {
        return Ok(());
    };
    if types.is_empty() {
        return Err(ManifestError::InvalidLibraryTypes(format!(
            "{section}.library_types 不得为空数组（缺省即 [\"music\"]）"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for ty in types {
        if !seen.insert(*ty) {
            return Err(ManifestError::InvalidLibraryTypes(format!(
                "{section}.library_types 含重复项 {:?}",
                ty.as_str()
            )));
        }
    }
    if abi_min < LIBRARY_TYPES_MIN_ABI {
        return Err(ManifestError::InvalidLibraryTypes(format!(
            "声明 {section}.library_types 要求 abi.min >= 1.5（当前 {abi_min:?}）"
        )));
    }
    Ok(())
}

/// `actions` 校验：id 字符集、列表内唯一、非空声明要求 `abi.min` ≥ 1.2。
fn validate_actions(
    actions: &[ActionManifest],
    abi_min: crate::abi::AbiVersion,
) -> Result<(), ManifestError> {
    let mut seen = std::collections::HashSet::new();
    for (index, action) in actions.iter().enumerate() {
        if !is_valid_action_id(&action.id) {
            return Err(ManifestError::InvalidActions(format!(
                "第 {index} 项 id {:?} 不合法（须 ^[a-z][a-z0-9_]{{0,63}}$）",
                action.id
            )));
        }
        if !seen.insert(&action.id) {
            return Err(ManifestError::InvalidActions(format!(
                "action id {:?} 在列表内重复",
                action.id
            )));
        }
    }
    if !actions.is_empty() && abi_min < ACTIONS_MIN_ABI {
        return Err(ManifestError::InvalidActions(format!(
            "声明非空 actions 要求 abi.min >= 1.2（当前 {abi_min:?}）"
        )));
    }
    Ok(())
}

/// `playlist_import` 校验：methods 非空 + ABI 门槛 + method id 字符集/唯一 +
/// `input_schema` 结构子集。
///
/// method id 与 action id 共用同一字符集（[`is_valid_action_id`]）：两者都会进
/// URL 路径与前端路由，两套规则若分叉必然漂移。
fn validate_playlist_import(
    section: &PlaylistImportManifest,
    abi_min: crate::abi::AbiVersion,
) -> Result<(), ManifestError> {
    if section.methods.is_empty() {
        return Err(ManifestError::InvalidPlaylistImport(
            "methods 不得为空（声明 playlist_import 却不提供任何方法属作者错误）".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for (index, method) in section.methods.iter().enumerate() {
        if !is_valid_action_id(&method.id) {
            return Err(ManifestError::InvalidPlaylistImport(format!(
                "第 {index} 项 id {:?} 不合法（须 ^[a-z][a-z0-9_]{{0,63}}$，与 action id 同字符集）",
                method.id
            )));
        }
        if !seen.insert(&method.id) {
            return Err(ManifestError::InvalidPlaylistImport(format!(
                "method id {:?} 在列表内重复",
                method.id
            )));
        }
        if let Some(schema) = &method.input_schema {
            validate_config_schema(schema, "input_schema")?;
        }
    }
    if abi_min < PLAYLIST_IMPORT_MIN_ABI {
        return Err(ManifestError::InvalidPlaylistImport(format!(
            "声明 playlist_import 要求 abi.min >= 1.4（当前 {abi_min:?}）"
        )));
    }
    Ok(())
}

/// 入站 HTTP 路由校验：路径形态、方法列表唯一性与 ABI 门槛。
fn validate_http_manifest(
    section: &HttpManifest,
    abi_min: crate::abi::AbiVersion,
) -> Result<(), ManifestError> {
    if section.routes.is_empty() {
        return Err(ManifestError::InvalidHttp(
            "routes 不得为空（声明 http 却不提供任何路由属作者错误）".into(),
        ));
    }
    let mut paths = std::collections::HashSet::new();
    for (index, route) in section.routes.iter().enumerate() {
        let path = route.path.as_str();
        if path != path.trim()
            || path.is_empty()
            || !path.starts_with('/')
            || path.contains('?')
            || path.contains('#')
        {
            return Err(ManifestError::InvalidHttp(format!(
                "第 {index} 项 path {:?} 不合法（须以 / 开头、无首尾空白且不得包含查询或片段）",
                route.path
            )));
        }
        if path.contains("//") || path.split('/').any(|segment| segment == "..") {
            return Err(ManifestError::InvalidHttp(format!(
                "第 {index} 项 path {:?} 含有空段或 .. 路径",
                route.path
            )));
        }
        if path.contains('*') && !path.ends_with("/*") {
            return Err(ManifestError::InvalidHttp(format!(
                "第 {index} 项 path {:?} 不合法（通配符只能出现在末尾的 /*）",
                route.path
            )));
        }
        if !paths.insert(path.to_string()) {
            return Err(ManifestError::InvalidHttp(format!(
                "路由 path {:?} 重复",
                route.path
            )));
        }
        if route.methods.is_empty() {
            return Err(ManifestError::InvalidHttp(format!(
                "第 {index} 项 methods 不得为空"
            )));
        }
        let mut methods = std::collections::HashSet::new();
        for method in &route.methods {
            if !methods.insert(*method) {
                return Err(ManifestError::InvalidHttp(format!(
                    "第 {index} 项 methods 含重复项 {}",
                    method.as_str()
                )));
            }
        }
    }
    if abi_min < HTTP_MIN_ABI {
        return Err(ManifestError::InvalidHttp(format!(
            "声明 http 要求 abi.min >= 1.6（当前 {abi_min:?}）"
        )));
    }
    Ok(())
}

/// `$` 键引用校验：收集 manifest 全部 `$` 引用，逐个必须能被 `i18n.en` 表解析。
///
/// 引用来源：config_schema/user_config_schema 属性的 `title`/`description`
/// （任意嵌套深度）、`actions[].label` 与 `playlist_import.methods[].label`；
/// 不以 `$` 开头视为字面文本（存量插件零迁移）。解析本身在前端做，这里只保证
/// en 表存在且含全部被引用键（悬空引用 = 作者错误）。
fn validate_i18n_refs(manifest: &PluginManifest) -> Result<(), ManifestError> {
    let mut refs: Vec<(String, String)> = Vec::new(); // (位置描述, 引用键)
    if let Some(schema) = &manifest.config_schema {
        collect_config_i18n_refs(schema, "config", "", &mut refs);
    }
    if let Some(schema) = &manifest.user_config_schema {
        collect_config_i18n_refs(schema, "user_config", "", &mut refs);
    }
    if let Some(actions) = &manifest.actions {
        for action in actions {
            if let Some(key) = action.label.strip_prefix('$') {
                refs.push((format!("actions.{}.label", action.id), key.to_string()));
            }
        }
    }
    if let Some(section) = &manifest.playlist_import {
        for method in &section.methods {
            if let Some(key) = method.label.strip_prefix('$') {
                refs.push((
                    format!("playlist_import.{}.label", method.id),
                    key.to_string(),
                ));
            }
        }
    }
    if refs.is_empty() {
        return Ok(());
    }
    let Some(en) = manifest.i18n.as_ref().and_then(|t| t.get("en")) else {
        return Err(ManifestError::DanglingI18nRef(
            "manifest 含 $ 引用但缺 i18n.en 表（$ 引用必须能被 en 表解析）".into(),
        ));
    };
    for (location, key) in refs {
        if !en.contains_key(&key) {
            return Err(ManifestError::DanglingI18nRef(format!(
                "{location} 的引用 ${key} 无法被 i18n.en 解析（缺键）"
            )));
        }
    }
    Ok(())
}

/// 递归收集 config 表单属性 `title`/`description` 里的 `$` 引用。
///
/// 位置按 `<表单前缀>.<点分属性路径>.title|description` 约定记录（全局配置为
/// `config`、每用户配置为 `user_config`，与前端解析的键路径约定一致）；
/// 遍历口径与 [`validate_config_schema`] 相同：只看 `properties` 条目，
/// 嵌套 object 分组递归。
fn collect_config_i18n_refs(
    schema: &serde_json::Value,
    form: &str,
    prefix: &str,
    refs: &mut Vec<(String, String)>,
) {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    for (name, prop) in props {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        for field in ["title", "description"] {
            if let Some(key) = prop
                .get(field)
                .and_then(|v| v.as_str())
                .and_then(|v| v.strip_prefix('$'))
            {
                refs.push((format!("{form}.{path}.{field}"), key.to_string()));
            }
        }
        if prop.get("type").and_then(|t| t.as_str()) == Some("object") {
            collect_config_i18n_refs(prop, form, &path, refs);
        }
    }
}

/// 插件 id 长度上限（`^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*$`，整体 1..=200 字符）。
const MAX_ID_LEN: usize = 200;

/// id 是否满足字符集与长度约束：段内仅 `[A-Za-z0-9_]`，点分段，段非空，总长不超上限。
///
/// 动机：管理面把插件包落盘为 `{id}-{version}.tmap`，卸载/升级时按 `{id}-`
/// 前缀清理旧版本文件（server 端 `remove_stale_versions`）。`-` 在域名标签里
/// 合法，但 id 含 `-` 会让前缀匹配产生歧义——清理 `com.my` 的旧版本会误删
/// `com.my-site.x` 的包——因此在清单校验层直接禁止 `-`、空格、`/`、非 ASCII
/// 及空段（前导/尾随/连续点）。
fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return false;
    }
    // 合法字符集仅 ASCII，按字节迭代等价于按字符；多字节 UTF-8 首字节 >= 0x80
    // 自然落入拒绝分支。
    let mut prev_dot = true; // 视作段首：首字符为点 = 前导点，拒绝
    for &b in id.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => prev_dot = false,
            b'.' => {
                if prev_dot {
                    return false; // 前导点或连续点
                }
                prev_dot = true;
            }
            _ => return false, // `-`、空格、`/` 等一切其他字符
        }
    }
    !prev_dot // 尾随点拒绝（空串已在入口拦截）
}

/// locale 是否满足 `^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$`（如 zh / en / zh-Hans / en-US）。
///
/// 宽松口径：只约束形态不约束语义（不维护语言库）；非 ASCII 首字节 >= 0x80
/// 自然落入拒绝分支。后续段长度下界 2 与 BCP-47 变体/书写子标签一致。
fn is_valid_locale(locale: &str) -> bool {
    fn is_ascii_alpha(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_uppercase()
    }
    let mut segments = locale.split('-');
    let primary = segments.next().unwrap_or_default();
    let primary_ok =
        (2..=3).contains(&primary.len()) && primary.bytes().all(|b| b.is_ascii_lowercase());
    if !primary_ok {
        return false;
    }
    segments.all(|seg| {
        (2..=8).contains(&seg.len()) && seg.bytes().all(|b| is_ascii_alpha(b) || b.is_ascii_digit())
    })
}

/// action id 是否满足 `^[a-z][a-z0-9_]{0,63}$`（小写开头，1..=64 字符）。
///
/// 动作 id 会进 URL 路径与前端路由，字符集从严（小写字母/数字/下划线）。
fn is_valid_action_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    match bytes.first() {
        Some(b'a'..=b'z') => {}
        _ => return false,
    }
    (1..=64).contains(&bytes.len())
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// config 表单受限子集（`config_schema`/`user_config_schema` 与导入方法的
/// `input_schema` 共用同一套规则）：
/// 顶层 object；属性仅 string/number/integer/boolean/enum/const，
/// 可嵌套 object 分组（递归同规则）；数组与 oneOf/anyOf/allOf 等组合子一律拒绝。
/// `readOnly: true` 表示由插件动作写入（如 `persist_secrets` 的 session_key），
/// 不进入用户表单，但仍可出现在 `required` 里作为运行时就绪门槛。
///
/// **属性 object 内的未知键一律放行**（如 `x-ui: "textarea"` 这类厂商扩展/UI
/// hint）：本函数只做结构校验，前向兼容新 hint 不必等宿主升级；取值层
/// （[`validate_config_values`]）同样忽略未知键。若未来要收紧，必须先评审
/// 存量插件（manifest_tests 里有显式单测锁死本行为）。
///
/// `field` 仅用于错误文案里的字段名，规则本身完全一致。
fn validate_config_schema(
    schema: &serde_json::Value,
    field: &'static str,
) -> Result<(), ManifestError> {
    let Some(obj) = schema.as_object() else {
        return Err(ManifestError::InvalidConfigSchema(format!(
            "{field} 必须是 object"
        )));
    };
    if let Some(kind) = obj.get("type")
        && kind.as_str() != Some("object")
    {
        return Err(ManifestError::InvalidConfigSchema(format!(
            "{field} 顶层 type 只允许 \"object\""
        )));
    }
    if let Some(required) = obj.get("required") {
        let ok = required
            .as_array()
            .is_some_and(|items| items.iter().all(|i| i.is_string()));
        if !ok {
            return Err(ManifestError::InvalidConfigSchema(
                "required 必须是字符串数组".into(),
            ));
        }
    }
    if let Some(props) = obj.get("properties") {
        let Some(props) = props.as_object() else {
            return Err(ManifestError::InvalidConfigSchema(
                "properties 必须是 object".into(),
            ));
        };
        for (name, prop) in props {
            validate_config_property(name, prop)?;
        }
    }
    Ok(())
}

fn validate_config_property(name: &str, prop: &serde_json::Value) -> Result<(), ManifestError> {
    let Some(obj) = prop.as_object() else {
        return Err(ManifestError::InvalidConfigSchema(format!(
            "属性 {name:?} 的定义必须是 object"
        )));
    };
    for combinator in ["oneOf", "anyOf", "allOf", "not"] {
        if obj.contains_key(combinator) {
            return Err(ManifestError::InvalidConfigSchema(format!(
                "属性 {name:?} 不支持组合子 {combinator}"
            )));
        }
    }
    if let Some(kind) = obj.get("type") {
        match kind.as_str() {
            Some("string") | Some("number") | Some("integer") | Some("boolean") => {}
            Some("object") => {
                return validate_config_schema(prop, "嵌套分组");
            }
            other => {
                return Err(ManifestError::InvalidConfigSchema(format!(
                    "属性 {name:?} 的 type 不支持 {other:?}（仅 string/number/integer/boolean/object）"
                )));
            }
        }
    }
    if let Some(enumeration) = obj.get("enum")
        && !enumeration.is_array()
    {
        return Err(ManifestError::InvalidConfigSchema(format!(
            "属性 {name:?} 的 enum 必须是数组"
        )));
    }
    if obj.get("type").is_none() && !obj.contains_key("enum") && !obj.contains_key("const") {
        return Err(ManifestError::InvalidConfigSchema(format!(
            "属性 {name:?} 缺少 type/enum/const 之一"
        )));
    }
    Ok(())
}

/// 配置实例值校验错误（管理面 PUT config 与宿主侧共用）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigValuesError {
    #[error("values 必须是 object")]
    NotObject,
    #[error("插件未声明 config_schema，不接受 values")]
    NoSchema,
    #[error("{0}")]
    Invalid(String),
}

/// 按 manifest `config_schema` 校验配置实例值（与结构校验 [`validate_config_schema`] 配套）。
///
/// - 顶层与嵌套 object 的未知键一律拒绝；
/// - type/enum/const 规则与管理面 PUT 行为对齐；
/// - 无 schema 且 values 为非空 object → [`ConfigValuesError::NoSchema`]。
pub fn validate_config_values(
    schema: Option<&serde_json::Value>,
    values: &serde_json::Value,
) -> Result<(), ConfigValuesError> {
    let Some(values_obj) = values.as_object() else {
        return Err(ConfigValuesError::NotObject);
    };
    let Some(schema) = schema else {
        if values_obj.is_empty() {
            return Ok(());
        }
        return Err(ConfigValuesError::NoSchema);
    };
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        if values_obj.is_empty() {
            return Ok(());
        }
        return Err(ConfigValuesError::NoSchema);
    };
    for (key, value) in values_obj {
        let Some(prop) = properties.get(key) else {
            return Err(ConfigValuesError::Invalid(format!(
                "未知配置项 {key:?}（schema 未声明）"
            )));
        };
        validate_config_value_type(key, prop, value)?;
    }
    Ok(())
}

fn validate_config_value_type(
    key: &str,
    prop: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), ConfigValuesError> {
    if prop.get("readOnly").and_then(|value| value.as_bool()) == Some(true) {
        return Err(ConfigValuesError::Invalid(format!(
            "配置项 {key:?} 是只读字段，不允许提交"
        )));
    }
    if let Some(enumeration) = prop.get("enum").and_then(|e| e.as_array())
        && !enumeration.contains(value)
    {
        return Err(ConfigValuesError::Invalid(format!(
            "配置项 {key:?} 的值不在 enum 限值内"
        )));
    }
    if let Some(constant) = prop.get("const")
        && constant != value
    {
        return Err(ConfigValuesError::Invalid(format!(
            "配置项 {key:?} 必须为 const 值"
        )));
    }
    match prop.get("type").and_then(|t| t.as_str()) {
        Some("string") => {
            if !value.is_string() {
                return Err(config_value_type_error(key, "string"));
            }
        }
        Some("number") => {
            if !value.is_number() {
                return Err(config_value_type_error(key, "number"));
            }
        }
        Some("integer") => {
            if !(value.is_i64() || value.is_u64()) {
                return Err(config_value_type_error(key, "integer"));
            }
        }
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(config_value_type_error(key, "boolean"));
            }
        }
        Some("object") => {
            if !value.is_object() {
                return Err(config_value_type_error(key, "object"));
            }
            if let (Some(sub_props), Some(sub_values)) = (
                prop.get("properties").and_then(|p| p.as_object()),
                value.as_object(),
            ) {
                for (sub_key, sub_value) in sub_values {
                    let sub_path = format!("{key}.{sub_key}");
                    let Some(sub_prop) = sub_props.get(sub_key) else {
                        return Err(ConfigValuesError::Invalid(format!(
                            "未知配置项 {sub_path:?}（schema 未声明）"
                        )));
                    };
                    validate_config_value_type(&sub_path, sub_prop, sub_value)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn config_value_type_error(key: &str, expected: &str) -> ConfigValuesError {
    ConfigValuesError::Invalid(format!("配置项 {key:?} 类型须为 {expected}"))
}

/// 清单校验错误（宿主拒绝加载插件时给出的结构化原因）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    #[error("plugin id must not be empty")]
    EmptyId,
    /// id 字符集/长度非法（含 `-`、空格、`/`、非 ASCII、空段或超 200 字符）。
    ///
    /// `-` 必须禁止：包文件按 `{id}-{version}.tmap` 命名并按 `{id}-` 前缀清理，
    /// id 含 `-` 会令前缀匹配误删他插件的包（如 `com.my` vs `com.my-site.x`）。
    #[error("invalid plugin id: {0}")]
    InvalidId(String),
    #[error("invalid plugin version: {0}")]
    InvalidVersion(String),
    #[error("abi range is empty: min {min:?} > max {max:?}")]
    EmptyAbiRange {
        min: crate::abi::AbiVersion,
        max: crate::abi::AbiVersion,
    },
    #[error("scrape_provider extension point requires nested `scrape` with `provider`")]
    MissingScrapeConfig,
    /// `scrobble_reporter` 扩展点缺嵌套配置（ABI 1.3 起）。
    #[error("scrobble_reporter extension point requires nested `scrobble_reporter` with `events`")]
    MissingScrobbleReporterConfig,
    /// `scrobble_reporter` 声明非法（events 为空 / ABI 门槛不满足）。
    #[error("invalid scrobble_reporter: {0}")]
    InvalidScrobbleReporter(String),
    /// `library_types` 声明非法（空数组 / 重复项 / ABI 门槛不满足）。
    #[error("invalid library_types: {0}")]
    InvalidLibraryTypes(String),
    /// `playlist_import` 扩展点缺嵌套配置（ABI 1.4 起）。
    #[error("playlist_import extension point requires nested `playlist_import` with `methods`")]
    MissingPlaylistImportConfig,
    /// `playlist_import` 声明非法（methods 为空 / method id 字符集或重复 /
    /// ABI 门槛不满足 / `input_schema` 超出受限子集）。
    #[error("invalid playlist_import: {0}")]
    InvalidPlaylistImport(String),
    /// `http` 扩展点声明非法（路由为空/路径或方法重复/ABI 门槛不满足）。
    #[error("invalid http routes: {0}")]
    InvalidHttp(String),
    /// `http` 扩展点缺嵌套配置（ABI 1.6 起）。
    #[error("http extension point requires nested `http` with `routes`")]
    MissingHttpConfig,
    /// 每用户配置声明非法（user_config_secrets 空条目 / ABI 门槛不满足）。
    #[error("invalid user config declaration: {0}")]
    InvalidUserConfig(String),
    /// 第 `index` 项权限结构非法（空 host/reason、host 夹带 scheme 等）。
    #[error("invalid permission at index {index}: {source}")]
    InvalidPermissionAt {
        index: usize,
        #[source]
        source: crate::permission::PermissionError,
    },
    /// `config_schema` 结构超出受限子集（数组/组合子/未知属性类型等）。
    #[error("invalid config_schema: {0}")]
    InvalidConfigSchema(String),
    /// `config_secrets` 条目非法（空字符串）。
    #[error("invalid config_secrets: {0}")]
    InvalidConfigSecrets(String),
    /// `i18n` 表结构非法（locale 键形态、空文案键）。
    #[error("invalid i18n: {0}")]
    InvalidI18n(String),
    /// `actions` 声明非法（id 字符集/重复/ABI 门槛不满足）。
    #[error("invalid actions: {0}")]
    InvalidActions(String),
    /// `$` i18n 引用悬空（缺 i18n.en 表或缺被引用键）。
    #[error("dangling i18n reference: {0}")]
    DanglingI18nRef(String),
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
