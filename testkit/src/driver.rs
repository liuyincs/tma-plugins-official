//! 已编译插件的调用驱动（原 `LoadedPlugin`，instance-per-call 语义不变）。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use extism::{Function, UserData, ValType};

use crate::host_fns::{
    HostFnState, granted_capabilities, http_request_host_fn, tma_catalog_read_host_fn,
    tma_config_host_fn, tma_identity_read_host_fn, tma_media_stream_host_fn,
};
use crate::{
    CapabilityStub, CatalogReadRequest, EntityQuery, IdentityReadRequest, MediaStreamRequest,
    PluginActionRequest, PluginActionResponse, PluginError, PluginErrorCode, PluginEventRequest,
    PluginEventResponse, PluginHttpRequest, PluginHttpResponse, PluginManifest, PluginOp,
    PluginOutcome, PluginRequest, PluginResponse, RecordedRequest, ScrapeResult, StubProxy,
    VerifiedPlugin,
};

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn internal_error(message: String) -> PluginError {
    PluginError::new(PluginErrorCode::Internal, message)
}

/// 完整 source chain（extism 错误常以 context 包装底层 trap，只看 to_string
/// 会丢掉真正原因）。
pub(crate) fn error_chain(e: &extism::Error) -> String {
    let mut out = e.to_string();
    for cause in e.chain().skip(1) {
        out.push_str(": ");
        out.push_str(&cause.to_string());
    }
    out
}

/// 已编译并绑定宿主函数的插件：并发安全，每次调用 instance-per-call 重实例化。
/// 构造入口收归 [`crate::HostSim`]（插件实例只能由 HostSim 造）；记录面
/// （stub 代理与 capability 桩的请求留痕）跨调用累计，经本类型的观察方法读出。
pub struct Driver {
    compiled: extism::CompiledPlugin,
    proxy: Arc<StubProxy>,
    capabilities: Arc<CapabilityStub>,
}

impl Driver {
    /// 直接从未验签 wasm 字节构造（`HostSim::load_wasm` 之下；清单无关路径可用）。
    ///
    /// `runtime_config` 为注入的运行时配置 JSON（未配置传 `"{}"`），实例化后
    /// 经 `tma_config` 宿主函数对插件可见。裸 wasm 路径无清单可据，capability
    /// 权限门全放行；要复现「清单未声明的 capability 被宿主拒」请走验签包
    /// 路径（[`Self::from_verified`]）。
    pub(crate) fn from_wasm(
        wasm: &[u8],
        runtime_config: Arc<str>,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
    ) -> Result<Self, PluginError> {
        Self::from_wasm_inner(wasm, runtime_config, proxy, capabilities, None)
    }

    /// 验签包 → 实例化 → `tma_manifest` 探测比对（存在导出即须与包内清单一致，
    /// 与宿主 `load_verified` 同语义）。除 [`Self::from_wasm`] 的行为外，还按
    /// `verified.manifest` 的 `permissions` 复现逐次 capability 权限门——插件
    /// 调用清单未声明的 capability 时宿主函数写回 `ok:false` + `forbidden`
    /// 应答（media 为 403），与真实宿主同口径。
    pub(crate) fn from_verified(
        verified: &VerifiedPlugin,
        runtime_config: Arc<str>,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
    ) -> Result<Self, PluginError> {
        let plugin = Self::from_wasm_inner(
            &verified.wasm,
            runtime_config,
            proxy,
            capabilities,
            Some(Arc::new(granted_capabilities(&verified.manifest))),
        )?;
        if let Some(exported) = plugin.probe_manifest()?
            && verified.manifest != exported
        {
            return Err(internal_error(
                "tma_manifest 导出与包内清单不一致，拒绝安装".into(),
            ));
        }
        Ok(plugin)
    }

    fn from_wasm_inner(
        wasm: &[u8],
        runtime_config: Arc<str>,
        proxy: Arc<StubProxy>,
        capabilities: Arc<CapabilityStub>,
        granted: Option<Arc<BTreeSet<String>>>,
    ) -> Result<Self, PluginError> {
        let state = HostFnState {
            proxy: proxy.clone(),
            capabilities: capabilities.clone(),
            runtime_config,
            granted,
        };
        let functions = vec![
            Function::new(
                "http_request",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                http_request_host_fn,
            ),
            Function::new(
                "tma_config",
                [],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_config_host_fn,
            ),
            Function::new(
                "tma_catalog_read",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_catalog_read_host_fn,
            ),
            Function::new(
                "tma_identity_read",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state.clone()),
                tma_identity_read_host_fn,
            ),
            Function::new(
                "tma_media_stream",
                [ValType::I64],
                [ValType::I64],
                UserData::new(state),
                tma_media_stream_host_fn,
            ),
        ];
        // 与原验收夹具的 WasmEngineConfig 一致：10s 超时、无 fuel、1024 页上限。
        let manifest = extism::Manifest::new([extism::Wasm::data(wasm.to_vec())])
            .with_timeout(Duration::from_secs(10))
            .with_memory_max(1024);
        let compiled = extism::PluginBuilder::new(manifest)
            .with_functions(functions)
            .compile()
            .map_err(|e| internal_error(format!("wasm 编译失败: {}", error_chain(&e))))?;
        Ok(Self {
            compiled,
            proxy,
            capabilities,
        })
    }

    /// 已记录的 `http_request` 出站请求（跨调用累计）。
    pub fn http_requests(&self) -> Vec<RecordedRequest> {
        self.proxy.requests()
    }

    /// 已记录的出站 URL 列表（`http_requests` 的便捷投影）。
    pub fn http_urls(&self) -> Vec<String> {
        self.proxy.urls()
    }

    /// 已记录的 `tma_catalog_read` 入参（含未命中调用；跨调用累计）。
    pub fn catalog_requests(&self) -> Vec<CatalogReadRequest> {
        self.capabilities.catalog_requests()
    }

    /// 已记录的 `tma_identity_read` 入参（含未配置调用；跨调用累计）。
    pub fn identity_requests(&self) -> Vec<IdentityReadRequest> {
        self.capabilities.identity_requests()
    }

    /// 已记录的 `tma_media_stream` 入参（含未命中调用；跨调用累计）。
    pub fn media_requests(&self) -> Vec<MediaStreamRequest> {
        self.capabilities.media_requests()
    }

    /// `tma_manifest` 导出探测（`Ok(None)` = 无该导出）。
    fn probe_manifest(&self) -> Result<Option<PluginManifest>, PluginError> {
        let mut plugin = self.instantiate()?;
        if !plugin.function_exists("tma_manifest") {
            return Ok(None);
        }
        let bytes = plugin
            .call::<&[u8], Vec<u8>>("tma_manifest", b"")
            .map_err(|e| internal_error(format!("tma_manifest 调用失败: {}", error_chain(&e))))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| internal_error(format!("tma_manifest 返回非法清单 JSON: {e}")))
    }

    fn instantiate(&self) -> Result<extism::Plugin, PluginError> {
        extism::Plugin::new_from_compiled(&self.compiled)
            .map_err(|e| internal_error(format!("wasm 实例化失败: {}", error_chain(&e))))
    }

    fn call_export(&self, export: &str, input: &[u8]) -> Result<Vec<u8>, PluginError> {
        let mut plugin = self.instantiate()?;
        plugin
            .call::<&[u8], Vec<u8>>(export, input)
            .map_err(|e| internal_error(format!("{export} 调用失败: {}", error_chain(&e))))
    }

    /// `scrape` 导出的信封调用（`PluginRequest`/`PluginResponse` 往返，
    /// 响应 id 必须原样带回——与宿主 `invoke` 同语义）。
    fn invoke(&self, op: PluginOp) -> Result<PluginOutcome, PluginError> {
        let req = PluginRequest {
            id: next_request_id(),
            op,
        };
        let input = serde_json::to_vec(&req)
            .map_err(|e| internal_error(format!("请求信封序列化失败: {e}")))?;
        let bytes = self.call_export("scrape", &input)?;
        let resp = serde_json::from_slice::<PluginResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法响应 JSON: {e}")))?;
        if resp.id != req.id {
            return Err(internal_error(format!(
                "响应信封 id 不匹配（期望 {}，实际 {}）",
                req.id, resp.id
            )));
        }
        match resp.outcome {
            PluginOutcome::Error(e) => Err(e),
            other => Ok(other),
        }
    }

    /// 单实体刮削（`scrape` 导出，`PluginOp::Scrape`）。
    pub fn call_scrape(&self, query: EntityQuery) -> Result<ScrapeResult, PluginError> {
        match self.invoke(PluginOp::Scrape { query })? {
            PluginOutcome::Scrape(result) => Ok(*result),
            PluginOutcome::Discography(_) => {
                Err(internal_error("scrape 请求收到 discography 响应".into()))
            }
            PluginOutcome::Error(e) => Err(e),
        }
    }

    /// `tma_action` 导出（无导出 → `Unsupported`，与宿主同语义）。
    pub fn call_action(
        &self,
        request: PluginActionRequest,
    ) -> Result<PluginActionResponse, PluginError> {
        if !self.export_exists("tma_action")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_action，不支持动作".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("动作请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_action", &input)?;
        serde_json::from_slice::<PluginActionResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法动作响应 JSON: {e}")))
    }

    /// `tma_event` 导出（无导出 → `Unsupported`，与宿主同语义）。
    pub fn call_event(
        &self,
        request: PluginEventRequest,
    ) -> Result<PluginEventResponse, PluginError> {
        if !self.export_exists("tma_event")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_event，不支持事件订阅".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("事件请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_event", &input)?;
        serde_json::from_slice::<PluginEventResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法事件响应 JSON: {e}")))
    }

    /// `tma_http` 导出（ABI 1.6 入站 HTTP 扩展点；无导出 → `Unsupported`，
    /// 与 `call_action`/`call_event` 同口径）。业务 `Err` 被插件折叠为非零
    /// 退出码，经 `call_export` 落为 Internal + 完整错误链。
    pub fn call_http(&self, request: PluginHttpRequest) -> Result<PluginHttpResponse, PluginError> {
        if !self.export_exists("tma_http")? {
            return Err(PluginError::new(
                PluginErrorCode::Unsupported,
                "插件未导出 tma_http，不支持入站 HTTP".into(),
            ));
        }
        let input = serde_json::to_vec(&request)
            .map_err(|e| internal_error(format!("HTTP 请求序列化失败: {e}")))?;
        let bytes = self.call_export("tma_http", &input)?;
        serde_json::from_slice::<PluginHttpResponse>(&bytes)
            .map_err(|e| internal_error(format!("插件返回非法 HTTP 响应 JSON: {e}")))
    }

    fn export_exists(&self, name: &str) -> Result<bool, PluginError> {
        let plugin = self.instantiate()?;
        Ok(plugin.function_exists(name))
    }
}
