//! 内置插件 `listenbrainz`：scrobble / now_playing 播放事件上报到
//! ListenBrainz（`https://api.listenbrainz.org/1/submit-listens`，0.1.0）。
//!
//! 模块布局与 `tma-builtin-lastfm` 同构（按 target 拆分）：
//!
//! - [`scrobble`]：上报纯逻辑（submit-listens 载荷组装、<30s 跳过判据、
//!   HTTP 状态 → PluginError 映射），**双 target 编译**——宿主
//!   `cargo test -p tma-builtin-listenbrainz` 直接跑其中的单测；
//! - [`plugin`]：wasm 插件本体（glue FFI 样板 + 事件/动作业务），仅 wasm
//!   target 编译（extism-pdk 引用的宿主函数只在 wasm target 存在，宿主编译
//!   必然链接失败；wasm 构建走 `scripts/build-builtin-plugins.sh`）。
//!
//! 本插件不接入刮削扩展点（`extension_points` 仅 `scrobble_reporter`），
//! glue 宏未声明 scrape 分支（宿主探测 scrape 导出时得到 Unsupported）。

/// 上报纯逻辑（双 target 编译；宿主跑单测，wasm 侧经 `crate::plugin` 引用）。
/// 宿主非测试 target 下唯一消费方（wasm 侧 plugin 模块）不编译，
/// allow(dead_code) 保持零警告（模块本体仍参与宿主编译，纯度违约在此暴露）。
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod scrobble;

/// wasm 插件本体：FFI 样板 + 事件/动作业务（仅 wasm target，见模块注释）。
#[cfg(target_arch = "wasm32")]
mod plugin;
