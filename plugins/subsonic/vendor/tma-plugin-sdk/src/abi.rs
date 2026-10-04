//! ABI 版本建模：区间而非只有下界。
//!
//! Jellyfin 的 `targetAbi` 只有下界，插件声明"4 字起"后在新版本宿主上大面积
//! 误判兼容。这里从第一天起就把兼容范围建模为 min/max **闭区间**：插件作者
//! 自我声明"验证过的宿主 ABI 范围"，宿主按 [`AbiRange::contains`] 判定放行。

use serde::{Deserialize, Serialize};

/// 单个 ABI 版本点。
///
/// `major` 破坏性变更（DTO 字段删除/语义反转），`minor` 增补性变更
/// （新增可选字段、新增枚举变体）；同 major 内高 minor 宿主兼容低 minor 插件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AbiVersion {
    pub major: u32,
    pub minor: u32,
}

impl AbiVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

/// 当前宿主实现的 ABI 版本（契约自身版本，随 DTO 演进递增）。
///
/// 1.1：清单新增可选 `permissions`（出站 HTTP 权限声明，见 [`crate::permission`]）。
/// 1.2：清单新增可选 `actions`/`i18n` 与可选 `tma_action` 导出（见 [`crate::manifest`]）。
/// 1.3：新增可选 `tma_event` 导出与事件订阅扩展点 `scrobble_reporter`
/// （见 [`crate::events`]），以及清单可选 `user_config_schema`/
/// `user_config_secrets`（每用户插件配置，见 [`crate::manifest`]）。
/// 1.4：新增歌单导入扩展点 `playlist_import`（清单可选 `playlist_import` 段与
/// [`crate::playlist_import`] 方法请求/响应 DTO）、AI 权限变体
/// `Permission::Ai`（[`crate::permission::Permission::Ai`]）与宿主单轮对话
/// 函数 `ai_chat` 的跨边界 JSON 协议（[`crate::ai_host`]）。
/// 1.5：刮削/上报清单段新增可选 `library_types`（[`crate::library_type::LibraryType`]
/// 闭集），刮削 `EntityQuery` 新增 `library_types` 数组，事件曲目载荷新增
/// `library_type` 单值。
///
/// HTTP 权限上的可选 `traffic`、以及 `PluginError.retry_scope` 属于缺省省略
/// 的增补 JSON 字段：旧清单/旧错误 JSON 仍可解析，未知键忽略，不构成 ABI
/// 门槛，也不升 minor。
///
/// 1.4/1.5 与前序各档同为**增补性 minor 升级**：不删改既有字段与语义，低版本
/// 宿主兼容的插件只差"是否使用新形态"，未声明新形态的清单不受任何影响；请求
/// DTO 无 `deny_unknown_fields`，旧 guest 忽略新增键。
///
/// 1.6：新增通用入站 HTTP 扩展点 `http`（清单 `http.routes` 与 `tma_http` 导出）。
/// 入站请求在 WASM 沙箱内完成协议翻译，宿主负责鉴权、请求体限制和响应头安全策略。
pub const HOST_ABI: AbiVersion = AbiVersion::new(1, 6);

/// 声明通用入站 HTTP 扩展点的最低 ABI。
pub const HTTP_MIN_ABI: AbiVersion = AbiVersion::new(1, 6);

/// 声明非空 `actions`（及 `i18n`/`tma_action` 配套形态）的最低 ABI。
///
/// 只挡"新字段配旧区间"的作者错误：清单校验用它判定 `abi.min` 下界，
/// 未声明 actions 的旧清单不受任何影响。
pub const ACTIONS_MIN_ABI: AbiVersion = AbiVersion::new(1, 2);

/// 声明事件订阅（`scrobble_reporter` 扩展点与 `tma_event` 导出）或
/// `user_config_schema`/非空 `user_config_secrets` 的最低 ABI。
///
/// 与 [`ACTIONS_MIN_ABI`] 同构：只挡"新字段配旧区间"的作者错误，
/// 未声明事件/每用户配置的旧清单不受任何影响。
pub const EVENTS_MIN_ABI: AbiVersion = AbiVersion::new(1, 3);

/// 声明歌单导入扩展点（`playlist_import` 与非空 `methods`）的最低 ABI。
///
/// 与 [`ACTIONS_MIN_ABI`]/[`EVENTS_MIN_ABI`] 同构：只挡"新字段配旧区间"的
/// 作者错误，未声明歌单导入的旧清单不受任何影响。
pub const PLAYLIST_IMPORT_MIN_ABI: AbiVersion = AbiVersion::new(1, 4);

/// 声明 `scrape.library_types` / `scrobble_reporter.library_types` 的最低 ABI。
///
/// 与 [`PLAYLIST_IMPORT_MIN_ABI`] 同构：只挡"新字段配旧区间"的作者错误，
/// 未声明该字段的旧清单不受任何影响（缺省 = `["music"]`）。
pub const LIBRARY_TYPES_MIN_ABI: AbiVersion = AbiVersion::new(1, 5);

/// 插件兼容的宿主 ABI 闭区间（含两端）。
///
/// `max` 是插件作者的保守上界（"只验证到这个版本"），不是宿主的承诺；
/// 宿主升级后超界的插件会被拒绝加载，由作者重新验证后放宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbiRange {
    pub min: AbiVersion,
    pub max: AbiVersion,
}

impl AbiRange {
    /// 单点区间：只兼容指定版本。
    pub const fn exactly(v: AbiVersion) -> Self {
        Self { min: v, max: v }
    }

    /// 兼容判定：宿主版本落在闭区间内（含两端）即放行。
    pub fn contains(&self, host: AbiVersion) -> bool {
        self.min <= host && host <= self.max
    }

    /// 区间自洽性：`min <= max`。空区间（min > max）是清单错误。
    pub fn is_valid(&self) -> bool {
        self.min <= self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_inclusive_on_both_ends() {
        let range = AbiRange {
            min: AbiVersion::new(1, 0),
            max: AbiVersion::new(1, 2),
        };
        assert!(range.contains(AbiVersion::new(1, 0)));
        assert!(range.contains(AbiVersion::new(1, 1)));
        assert!(range.contains(AbiVersion::new(1, 2)));
        assert!(!range.contains(AbiVersion::new(0, 9)));
        assert!(!range.contains(AbiVersion::new(1, 3)));
        assert!(!range.contains(AbiVersion::new(2, 0)));
    }

    /// 只写下界的版本地牢反例：声明 [1.0, +∞) 的插件在宿主 2.0 上仍会被判兼容。
    #[test]
    fn range_bounds_major_bumps() {
        let v1 = AbiVersion::new(1, 0);
        let v2 = AbiVersion::new(2, 0);
        let wide = AbiRange {
            min: AbiVersion::new(1, 0),
            max: AbiVersion::new(1, 0),
        };
        assert!(wide.contains(v1));
        assert!(!wide.contains(v2), "major 跳变必须挡在区间外");
    }

    #[test]
    fn ordering_is_major_then_minor() {
        assert!(AbiVersion::new(1, 2) < AbiVersion::new(1, 3));
        assert!(AbiVersion::new(1, 9) < AbiVersion::new(2, 0));
        assert_eq!(AbiVersion::new(1, 0), AbiVersion::new(1, 0));
    }

    #[test]
    fn exactly_helper() {
        let v = AbiVersion::new(1, 1);
        assert!(AbiRange::exactly(v).contains(v));
        assert!(!AbiRange::exactly(v).contains(AbiVersion::new(1, 2)));
    }

    /// actions 门槛：1.2，且宿主自身版本必须满足（否则 1.2 宿主装不了 actions 插件）。
    #[test]
    fn actions_min_abi_is_1_2_and_host_satisfies_it() {
        assert_eq!(ACTIONS_MIN_ABI, AbiVersion::new(1, 2));
        assert!(HOST_ABI >= ACTIONS_MIN_ABI);
    }

    /// 事件/每用户配置门槛：1.3，且宿主自身版本必须满足（否则 1.3 宿主装不了
    /// 事件订阅插件——与 actions 门槛同构的守卫）。
    #[test]
    fn events_min_abi_is_1_3_and_host_satisfies_it() {
        assert_eq!(EVENTS_MIN_ABI, AbiVersion::new(1, 3));
        assert!(HOST_ABI >= EVENTS_MIN_ABI);
        // 1.2 宿主满足 actions 门槛但不含事件契约：两个门槛各自独立递增。
        assert!(EVENTS_MIN_ABI > ACTIONS_MIN_ABI);
    }

    /// 歌单导入门槛：1.4，且宿主自身版本必须满足（否则 1.4 宿主装不了
    /// 歌单导入插件——与 events 门槛同构的守卫）。
    #[test]
    fn playlist_import_min_abi_is_1_4_and_host_satisfies_it() {
        assert_eq!(PLAYLIST_IMPORT_MIN_ABI, AbiVersion::new(1, 4));
        assert!(HOST_ABI >= PLAYLIST_IMPORT_MIN_ABI);
        // 1.3 宿主满足事件门槛但不含歌单导入契约：各档门槛独立递增。
        assert!(PLAYLIST_IMPORT_MIN_ABI > EVENTS_MIN_ABI);
    }

    /// 资料库类型声明门槛：1.5，且宿主自身版本必须满足。
    #[test]
    fn library_types_min_abi_is_1_5_and_host_satisfies_it() {
        assert_eq!(LIBRARY_TYPES_MIN_ABI, AbiVersion::new(1, 5));
        assert!(HOST_ABI >= LIBRARY_TYPES_MIN_ABI);
        // 1.4 宿主满足歌单导入门槛但不含资料库类型契约：各档门槛独立递增。
        assert!(LIBRARY_TYPES_MIN_ABI > PLAYLIST_IMPORT_MIN_ABI);
    }

    #[test]
    fn empty_range_is_invalid() {
        let bad = AbiRange {
            min: AbiVersion::new(1, 2),
            max: AbiVersion::new(1, 0),
        };
        assert!(!bad.is_valid());
        assert!(
            AbiRange {
                min: AbiVersion::new(1, 0),
                max: HOST_ABI,
            }
            .is_valid()
        );
    }

    #[test]
    fn abi_version_serde_roundtrip() {
        let v = AbiVersion::new(1, 2);
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"{"major":1,"minor":2}"#);
        assert_eq!(serde_json::from_str::<AbiVersion>(&json).unwrap(), v);
    }

    #[test]
    fn abi_range_serde_roundtrip() {
        let r = AbiRange {
            min: AbiVersion::new(1, 0),
            max: AbiVersion::new(1, 4),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(
            json,
            r#"{"min":{"major":1,"minor":0},"max":{"major":1,"minor":4}}"#
        );
        assert_eq!(serde_json::from_str::<AbiRange>(&json).unwrap(), r);
    }
}
