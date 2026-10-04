//! 内置来源插件 `lastfm`：艺术家/专辑简介、图片与外部 ID（需 api_key），
//! 以及 scrobble / now_playing 播放事件上报（需用户级授权，0.3.0 起）。
//!
//! 模块布局按 target 拆分：
//!
//! - [`scrobble`]：上报纯逻辑（api_sig 签名、track.scrobble / track.updateNowPlaying /
//!   auth.getToken / auth.getSession 参数组装、body error 码表映射、<30s 跳过判据），**双 target 编译**——
//!   宿主 `cargo test -p tma-builtin-lastfm` 直接跑其中的单测；
//! - [`plugin`]：wasm 插件本体（glue FFI 样板 + 刮削实现 + 事件/动作业务），
//!   仅 wasm target 编译（extism-pdk 引用的宿主函数只在 wasm target 存在，
//!   宿主编译必然链接失败；wasm 构建走 `scripts/build-builtin-plugins.sh`）。
//!
//! 刮削实现（`plugin.rs`）移植自原生 `crates/scrape/src/providers/lastfm.rs`
//! （该文件即规格）：artist.getInfo / album.getInfo 与 artist.search / album.search
//! 共 4 个方法，api_key 走 query 参数，bio 空时回退英文重取。清单 slug 与落库
//! provider/source 字面量统一为 `lastfm`。

/// 上报纯逻辑（双 target 编译；宿主跑单测，wasm 侧经 `crate::plugin` 引用）。
/// 宿主非测试 target 下唯一消费方（wasm 侧 plugin 模块）不编译，
/// allow(dead_code) 保持零警告（模块本体仍参与宿主编译，纯度违约在此暴露）。
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod scrobble;

/// wasm 插件本体：FFI 样板 + 刮削/事件/动作业务（仅 wasm target，见模块注释）。
#[cfg(target_arch = "wasm32")]
mod plugin;
