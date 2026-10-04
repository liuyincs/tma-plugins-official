//! TMA 官方插件的验收测试共享件（`testkit`）。
//!
//! 用法：在插件目录的 `tests/` 里
//!
//! ```ignore
//! let proxy = StubProxy::new(vec![RouteBuilder::json("api.example.com", json!({...}))]);
//! let mut plugin = testkit::load_dir(
//!     std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
//!     r#"{"api_key":"k"}"#,
//!     proxy.clone(),
//! );
//! let r = plugin.call_scrape(testkit::artist_query(None, Some("X"))).unwrap();
//! ```
//!
//! `load_dir` 走「真实签名包」链路：`cargo build --target wasm32-unknown-unknown
//! --release`（增量缓存）→ 测试私钥 `pack` → `verify` 验签 → extism 实例化，
//! 与宿主安装路径同源（`tma-plugin-sdk` 的 `package` feature 即打包/验签实现）。
//!
//! 与真实宿主（`tma-plugin-host`）的保真边界：
//!
//! - `http_request` 宿主函数（`extism:host/user`）：入参/出参按
//!   `tma_plugin_sdk::http_host` 的 `HttpHostRequest`/`HttpHostResponse` JSON
//!   协议逐字段复现——i64 offset → JSON 入参 → `body_b64` 解码 → 响应
//!   `status`/`headers`/`body_b64` 编码 → 写回 offset；panic 兜底为
//!   `ok:false` 响应而非 trap；
//! - `tma_config` 宿主函数：无参，原样返回调用方注入的 config JSON 字符串；
//! - instance-per-call：每次导出调用都新建 extism 实例（宿主同语义）；
//! - **刻意不复现**：manifest `permissions` 的逐次权限门、3xx 重定向跟随、
//!   fuel/epoch 打断——stub 代理是唯一的出站注入点，未命中路由直接以
//!   `code:"network"` 显式报错（与宿主 `ProxyHttpError::Network` 同码同文）。
//!
//! `TEST_SIGNING_KEY_B64` 是测试专用密钥对（生成后即冻结）。它只服务本仓
//! 验收测试的自洽链路，**不是任何环境的真实签名私钥**，也绝不应被配置为
//! 宿主受信公钥。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};
use extism::{CurrentPlugin, Function, UserData, Val, ValType};
use tma_plugin_sdk::{
    EntityQuery, FetchedId, HttpHostRequest, HttpHostResponse, PluginActionRequest,
    PluginActionResponse, PluginError, PluginErrorCode, PluginEventRequest, PluginEventResponse,
    PluginIcon, PluginManifest, PluginOp, PluginOutcome, PluginRequest, PluginResponse,
    ScrapeEntityKind, ScrapeResult, VerifiedPlugin,
};

// ---------------------------------------------------------------------------
// stub 代理：URL 子串匹配的罐头响应 + 请求记录
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

/// 单条路由：URL 含 `needle` 即命中（首个命中优先）。
pub struct Route {
    pub needle: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub struct RouteBuilder;

impl RouteBuilder {
    /// 200 + JSON body 的罐头响应（等价原 `builtin_tests::support::RouteBuilder::json`）。
    pub fn json(needle: &str, body: serde_json::Value) -> Route {
        Route {
            needle: needle.to_string(),
            status: 200,
            headers: BTreeMap::new(),
            body: body.to_string(),
        }
    }
}

/// 记录型 stub 代理：未命中路由 → `network` 失败（测试将失败并带出 URL，
/// 与宿主 `ProxyHttpError::Network` 同码同文）。
pub struct StubProxy {
    routes: Vec<Route>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl StubProxy {
    pub fn new(routes: Vec<Route>) -> Arc<Self> {
        Arc::new(Self {
            routes,
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.url).collect()
    }

    /// `http_request` 宿主函数的业务段：先记录再匹配（未命中也留痕）。
    fn dispatch(
        &self,
        method: String,
        url: String,
        headers: BTreeMap<String, String>,
        body: Option<Vec<u8>>,
    ) -> HttpHostResponse {
        self.requests.lock().unwrap().push(RecordedRequest {
            method: method.clone(),
            url: url.clone(),
            headers,
            body,
        });
        let Some(route) = self.routes.iter().find(|r| url.contains(&r.needle)) else {
            // 宿主侧 `ProxyHttpError::Network(msg)` → code "network"、
            // error 为 Display 文本 "network: {msg}"，逐字对齐。
            return HttpHostResponse::failure(
                "network",
                format!("network: stub 代理无预设响应: {method} {url}"),
            );
        };
        HttpHostResponse::success(
            route.status,
            route.headers.clone(),
            BASE64.encode(route.body.as_bytes()),
        )
    }
}

// ---------------------------------------------------------------------------
// 宿主函数桩（注册到 extism 默认 `extism:host/user` 命名空间）
// ---------------------------------------------------------------------------

/// 宿主函数 user data：stub 代理 + 注入的运行时配置 JSON。
#[derive(Clone)]
struct HostFnState {
    proxy: Arc<StubProxy>,
    runtime_config: Arc<str>,
}

/// `http_request`（I64 offset 入 → I64 offset 出）：入参/出参映射与宿主
/// `http_request_host_fn` 逐字段一致；业务段把「带权限门与重定向的异步代理」
/// 替换为同步 [`StubProxy::dispatch`]，其余（offset 读取/释放、JSON 与
/// body_b64 编解码、panic 兜底为 `ok:false`）原样复现。
fn http_request_host_fn(
    plugin: &mut CurrentPlugin,
    input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    let out = std::panic::catch_unwind(AssertUnwindSafe(|| -> HttpHostResponse {
        let state = match user_data.get().and_then(|cell| {
            cell.lock()
                .map_err(|_| extism::Error::msg("user data 锁中毒"))
                .map(|guard| guard.clone())
        }) {
            Ok(s) => s,
            Err(e) => {
                return HttpHostResponse::failure("internal", format!("user data 不可用: {e}"));
            }
        };
        let offset = match input.first().and_then(|v| v.i64()) {
            Some(o) if o > 0 => o as u64,
            _ => {
                return HttpHostResponse::failure(
                    "bad_request",
                    "http_request 需要 i64 入参 offset",
                );
            }
        };
        let handle = match plugin.memory_handle(offset) {
            Some(h) => h,
            None => {
                return HttpHostResponse::failure(
                    "bad_request",
                    format!("非法入参 offset: {offset}"),
                );
            }
        };
        let bytes = match plugin.memory_bytes(handle) {
            Ok(b) => b.to_vec(),
            Err(e) => {
                return HttpHostResponse::failure("bad_request", format!("读取入参失败: {e}"));
            }
        };
        if let Err(e) = plugin.memory_free(handle) {
            return HttpHostResponse::failure("internal", format!("释放入参失败: {e}"));
        }

        let call: HttpHostRequest = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(e) => {
                return HttpHostResponse::failure("bad_request", format!("入参 JSON 非法: {e}"));
            }
        };

        let body = match call.body_b64.as_deref() {
            None | Some("") => None,
            Some(b64) => match BASE64.decode(b64) {
                Ok(b) => Some(b),
                Err(e) => {
                    return HttpHostResponse::failure(
                        "bad_request",
                        format!("body_b64 非法: {e}"),
                    );
                }
            },
        };

        state
            .proxy
            .dispatch(call.method, call.url, call.headers, body)
    }));

    let out = match out {
        Ok(v) => v,
        Err(_) => HttpHostResponse::failure("internal", "http_request 宿主函数 panic（已兜底）"),
    };
    let bytes = serde_json::to_vec(&out)
        .map_err(|e| extism::Error::msg(format!("http_request 响应序列化失败: {e}")))?;
    let mem = plugin
        .memory_new(bytes)
        .map_err(|e| e.context("http_request 写回内存失败"))?;
    if let Some(out) = output.first_mut() {
        *out = Val::I64(mem.offset() as i64);
    }
    Ok(())
}

/// `tma_config`（无参 → I64 offset）：返回注入的运行时配置 JSON 字符串
/// （与宿主 `tma_config_host_fn` 同语义；插件侧 `load_config` 对 `{}`/空串兜底）。
fn tma_config_host_fn(
    plugin: &mut CurrentPlugin,
    _input: &[Val],
    output: &mut [Val],
    user_data: UserData<HostFnState>,
) -> Result<(), extism::Error> {
    let state = user_data
        .get()
        .and_then(|cell| {
            cell.lock()
                .map_err(|_| extism::Error::msg("user data 锁中毒"))
                .map(|guard| guard.clone())
        })
        .map_err(|e| extism::Error::msg(format!("tma_config: user data 不可用: {e}")))?;
    let bytes = state.runtime_config.as_bytes().to_vec();
    let mem = plugin
        .memory_new(bytes)
        .map_err(|e| e.context("tma_config 写回内存失败"))?;
    if let Some(out) = output.first_mut() {
        *out = Val::I64(mem.offset() as i64);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试签名密钥对（本仓验收测试专用，勿作任何真实分发签名/受信公钥）
// ---------------------------------------------------------------------------

/// 测试专用 ed25519 私钥（base64 32 字节种子，`tma-plugin-dev keygen` 现生成后
/// 冻结在此）。仅用于 `pack_dir`/`verify` 的自洽链路——不是生产签名私钥，
/// 也不应进入任何宿主的受信公钥集合。
pub const TEST_SIGNING_KEY_B64: &str = "f3ctCS9uN2C3TqFwsZ69XgN2laeePKZpzhZqJLGjIns=";

/// 测试专用签名私钥（从 [`TEST_SIGNING_KEY_B64`] 解码）。
pub fn test_signing_key() -> SigningKey {
    let bytes: [u8; 32] = BASE64
        .decode(TEST_SIGNING_KEY_B64)
        .expect("测试私钥必须是合法 base64")
        .try_into()
        .expect("测试私钥必须是 32 字节 ed25519 种子");
    SigningKey::from_bytes(&bytes)
}

/// 与测试私钥配对的公钥（`verify` 的受信集合）。
pub fn test_verifying_key() -> VerifyingKey {
    test_signing_key().verifying_key()
}

// ---------------------------------------------------------------------------
// 打包 / 验签 / 加载
// ---------------------------------------------------------------------------

/// 确保插件 wasm 产物存在：`cargo build --target wasm32-unknown-unknown
/// --release`（cargo 增量缓存，重复调用近乎无成本）。返回产物路径。
pub fn ensure_wasm_built(plugin_dir: &Path) -> PathBuf {
    let toml = std::fs::read_to_string(plugin_dir.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("读 {:?}/Cargo.toml 失败: {e}", plugin_dir));
    // wasm 产物名 = crate 名连字符转下划线；crate 名取 Cargo.toml 首个
    // `name = "..."`（与 scripts/build-package.sh 同口径）。
    let crate_name = toml
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or_else(|| panic!("无法从 {:?}/Cargo.toml 解析 package name", plugin_dir))
        .to_string();
    let wasm = plugin_dir.join(format!(
        "target/wasm32-unknown-unknown/release/{}.wasm",
        crate_name.replace('-', "_")
    ));
    let status = std::process::Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(plugin_dir)
        .status()
        .expect("启动 cargo build 失败（需要 wasm32-unknown-unknown target）");
    assert!(
        status.success(),
        "cargo build --target wasm32-unknown-unknown --release 失败（{plugin_dir:?}）"
    );
    assert!(wasm.is_file(), "缺少 {wasm:?}（crate {crate_name}）");
    wasm
}

/// 读可选图标条目（`icon.svg` 优先、`icon.png` 其次，至多一个——与包格式一致）。
fn read_icon(plugin_dir: &Path) -> Option<PluginIcon> {
    for name in ["icon.svg", "icon.png"] {
        if let Ok(bytes) = std::fs::read(plugin_dir.join(name)) {
            return Some(PluginIcon { name, bytes });
        }
    }
    None
}

/// 把插件目录打成真实签名的 `.tmap`：构建 wasm → `manifest.json` 原始字节 +
/// wasm + 可选图标 → 测试私钥 `pack`。
pub fn pack_dir(plugin_dir: &Path) -> Result<Vec<u8>, tma_plugin_sdk::PackageError> {
    let wasm_path = ensure_wasm_built(plugin_dir);
    let manifest = std::fs::read(plugin_dir.join("manifest.json"))
        .unwrap_or_else(|e| panic!("读 {:?}/manifest.json 失败: {e}", plugin_dir));
    let wasm = std::fs::read(&wasm_path)
        .unwrap_or_else(|e| panic!("读 {wasm_path:?} 失败: {e}"));
    let key = test_signing_key();
    match read_icon(plugin_dir) {
        Some(icon) => tma_plugin_sdk::pack_with_icon(&manifest, &wasm, &icon, &key),
        None => tma_plugin_sdk::pack(&manifest, &wasm, &key),
    }
}

/// 验签 `.tmap` 字节（受信集合 = [`test_verifying_key`]）：解包 → 验签 →
/// 清单校验 → ABI 兼容，与宿主安装入口同源。
pub fn verify(tmap_bytes: &[u8]) -> Result<VerifiedPlugin, tma_plugin_sdk::PackageError> {
    tma_plugin_sdk::verify(tmap_bytes, &[test_verifying_key()])
}

fn next_request_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn internal_error(message: String) -> PluginError {
    PluginError::new(PluginErrorCode::Internal, message)
}

/// 完整 source chain（extism 错误常以 context 包装底层 trap，只看 to_string
/// 会丢掉真正原因）。
fn error_chain(e: &extism::Error) -> String {
    let mut out = e.to_string();
    for cause in e.chain().skip(1) {
        out.push_str(": ");
        out.push_str(&cause.to_string());
    }
    out
}

/// 已编译并绑定宿主函数的插件：并发安全，每次调用 instance-per-call 重实例化。
pub struct LoadedPlugin {
    compiled: extism::CompiledPlugin,
}

impl LoadedPlugin {
    /// 直接从未验签 wasm 字节构造（`load_verified` 之下；清单无关路径可用）。
    ///
    /// `runtime_config` 为注入的运行时配置 JSON（未配置传 `"{}"`），实例化后
    /// 经 `tma_config` 宿主函数对插件可见。
    pub fn from_wasm(
        wasm: &[u8],
        runtime_config: &str,
        proxy: Arc<StubProxy>,
    ) -> Result<Self, PluginError> {
        let state = HostFnState {
            proxy,
            runtime_config: Arc::from(runtime_config),
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
                UserData::new(state),
                tma_config_host_fn,
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
        Ok(Self { compiled })
    }

    /// 验签包 → 实例化 → `tma_manifest` 探测比对（存在导出即须与包内清单一致，
    /// 与宿主 `load_verified` 同语义）。
    pub fn from_verified(
        verified: &VerifiedPlugin,
        runtime_config: &str,
        proxy: Arc<StubProxy>,
    ) -> Result<Self, PluginError> {
        let plugin = Self::from_wasm(&verified.wasm, runtime_config, proxy)?;
        if let Some(exported) = plugin.probe_manifest()? {
            if verified.manifest != exported {
                return Err(internal_error(
                    "tma_manifest 导出与包内清单不一致，拒绝安装".into(),
                ));
            }
        }
        Ok(plugin)
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
            PluginOutcome::Discography(_) => Err(internal_error(
                "scrape 请求收到 discography 响应".into(),
            )),
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

    fn export_exists(&self, name: &str) -> Result<bool, PluginError> {
        let plugin = self.instantiate()?;
        Ok(plugin.function_exists(name))
    }
}

/// 一条命令到底的加载：构建 wasm → pack → verify → 实例化（含 manifest 探测）。
/// 任一步失败带上下文 panic——夹具层的失败本来就该让测试红掉。
pub fn load_dir(plugin_dir: &Path, runtime_config: &str, proxy: Arc<StubProxy>) -> LoadedPlugin {
    let tmap = pack_dir(plugin_dir)
        .unwrap_or_else(|e| panic!("{plugin_dir:?} 打包失败: {e}"));
    let verified = verify(&tmap)
        .unwrap_or_else(|e| panic!("{plugin_dir:?} 验签失败: {e}"));
    LoadedPlugin::from_verified(&verified, runtime_config, proxy)
        .unwrap_or_else(|e| panic!("{plugin_dir:?} 实例化失败: {e:?}"))
}

// ---------------------------------------------------------------------------
// 查询构造夹具（与原 `builtin_tests::support` 同名同形状）
// ---------------------------------------------------------------------------

pub fn artist_query(mbid: Option<&str>, name: Option<&str>) -> EntityQuery {
    EntityQuery {
        kind: ScrapeEntityKind::Artist,
        mbid: mbid.map(str::to_string),
        isrc: None,
        name: name.map(str::to_string),
        artist_name: None,
        known_external_ids: Vec::new(),
        library_types: Vec::new(),
    }
}

pub fn artist_query_with_known(
    mbid: Option<&str>,
    name: Option<&str>,
    known: &[(&str, &str)],
) -> EntityQuery {
    EntityQuery {
        known_external_ids: known
            .iter()
            .map(|(p, id)| FetchedId {
                provider: p.to_string(),
                external_id: id.to_string(),
                url: None,
            })
            .collect(),
        ..artist_query(mbid, name)
    }
}

pub const BEATLES_MBID: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d260c2";
pub const ABBEY_RG_MBID: &str = "b588555c-e1a7-45b1-8f91-2048a8393b58";
pub const ABBEY_RELEASE_MBID: &str = "22222222-2222-2222-2222-222222222222";

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试密钥对：私钥解码与公钥派生往返一致。
    #[test]
    fn test_keypair_decodes() {
        assert_eq!(test_signing_key().verifying_key(), test_verifying_key());
    }

    /// pack → verify 全链路自洽：最小合法清单 + 最小 wasm 模块（`\0asm` 头）。
    #[test]
    fn pack_verify_roundtrip() {
        let manifest = br#"{
            "id": "tma.test.echo",
            "name": "Echo",
            "version": "0.1.0",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 6 } },
            "extension_points": ["scrape_provider"],
            "scrape": {
                "provider": "echo_images",
                "capabilities": ["image"],
                "requires_credentials": false
            }
        }"#;
        let wasm = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let packed = tma_plugin_sdk::pack(manifest, &wasm, &test_signing_key()).unwrap();
        let verified = verify(&packed).unwrap();
        assert_eq!(verified.manifest.id, "tma.test.echo");
        assert_eq!(verified.wasm, wasm);
    }

    /// 未命中路由：`code:"network"` + 错误文案与宿主 `ProxyHttpError::Network`
    /// Display 同文，且请求仍被记录（断言行可见未命中 URL）。
    #[test]
    fn stub_proxy_miss_records_and_reports_network() {
        let proxy = StubProxy::new(vec![]);
        let resp = proxy.dispatch(
            "GET".into(),
            "https://example.com/x".into(),
            BTreeMap::new(),
            None,
        );
        assert!(!resp.ok);
        assert_eq!(resp.code.as_deref(), Some("network"));
        assert_eq!(
            resp.error.as_deref(),
            Some("network: stub 代理无预设响应: GET https://example.com/x")
        );
        assert_eq!(proxy.urls(), ["https://example.com/x"]);
    }
}
