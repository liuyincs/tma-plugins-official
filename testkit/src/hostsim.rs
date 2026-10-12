//! 插件测试的唯一装载入口：注入面配置 → [`Driver`]。

use std::path::Path;
use std::sync::Arc;

use crate::driver::Driver;
use crate::package::{pack_dir, verify};
use crate::{CapabilityStub, PluginError, StubProxy};

/// 插件测试的唯一接口：builder 式收拢注入面（运行时配置 JSON、stub 代理、
/// capability 桩），再经 `load_*` 系列产出 [`Driver`]。夹具层的装载失败
/// （打包/验签/实例化）一律 panic——那是测试自身坏了，该红就红。
///
/// ```ignore
/// let driver = HostSim::new()
///     .config(r#"{"api_key":"k"}"#)
///     .proxy(proxy)
///     .capabilities(stub)
///     .load_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
/// let r = driver.call_scrape(testkit::artist_query(None, Some("X"))).unwrap();
/// ```
pub struct HostSim {
    runtime_config: Arc<str>,
    proxy: Arc<StubProxy>,
    capabilities: Arc<CapabilityStub>,
}

impl Default for HostSim {
    fn default() -> Self {
        Self::new()
    }
}

impl HostSim {
    /// 空配置 + 空代理 + 全未配置空桩（三条 capability 宿主函数照常注册）。
    pub fn new() -> Self {
        Self {
            runtime_config: Arc::from("{}"),
            proxy: StubProxy::new(vec![]),
            capabilities: CapabilityStub::new(),
        }
    }

    /// 注入运行时配置 JSON（实例化后经 `tma_config` 宿主函数对插件可见）。
    pub fn config(mut self, json: impl Into<String>) -> Self {
        self.runtime_config = Into::<String>::into(json).into();
        self
    }

    /// 注入 stub 代理（`http_request` 出站的唯一注入点）。
    pub fn proxy(mut self, proxy: Arc<StubProxy>) -> Self {
        self.proxy = proxy;
        self
    }

    /// 注入 capability 桩（`tma_catalog_read`/`tma_identity_read`/
    /// `tma_media_stream` 的应答编排面）。
    pub fn capabilities(mut self, stub: Arc<CapabilityStub>) -> Self {
        self.capabilities = stub;
        self
    }

    /// 一条命令到底的装载：构建 wasm → pack → verify → [`Driver`]（含
    /// manifest 探测比对）。任一步失败带上下文 panic——夹具层的失败本来
    /// 就该让测试红掉。逐次 capability 权限门按 manifest `permissions` 生效。
    pub fn load_dir(self, dir: &Path) -> Driver {
        let tmap = pack_dir(dir).unwrap_or_else(|e| panic!("{dir:?} 打包失败: {e}"));
        let verified = verify(&tmap).unwrap_or_else(|e| panic!("{dir:?} 验签失败: {e}"));
        self.load_verified(&verified)
            .unwrap_or_else(|e| panic!("{dir:?} 装载失败: {e:?}"))
    }

    /// 验签后的插件 → [`Driver`]（Result 语义，供加载期拒绝类断言）：
    /// manifest 探测比对 + 逐次 capability 权限门按清单生效。
    pub fn load_verified(self, verified: &crate::VerifiedPlugin) -> Result<Driver, PluginError> {
        Driver::from_verified(
            verified,
            self.runtime_config.clone(),
            self.proxy,
            self.capabilities,
        )
    }

    /// 裸 wasm 字节 → [`Driver`]：无清单路径，capability 权限门全放行
    /// （不走 manifest 探测比对）。
    pub fn load_wasm(self, wasm: &[u8]) -> Result<Driver, PluginError> {
        Driver::from_wasm(
            wasm,
            self.runtime_config.clone(),
            self.proxy,
            self.capabilities,
        )
    }
}
