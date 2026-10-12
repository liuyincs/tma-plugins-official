//! 测试签名密钥对与打包 / 验签（与宿主安装路径同源）。

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};

use crate::PluginIcon;

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
    let wasm = std::fs::read(&wasm_path).unwrap_or_else(|e| panic!("读 {wasm_path:?} 失败: {e}"));
    let key = test_signing_key();
    match read_icon(plugin_dir) {
        Some(icon) => tma_plugin_sdk::pack_with_icon(&manifest, &wasm, &icon, &key),
        None => tma_plugin_sdk::pack(&manifest, &wasm, &key),
    }
}

/// 验签 `.tmap` 字节（受信集合 = [`test_verifying_key`]）：解包 → 验签 →
/// 清单校验 → ABI 兼容，与宿主安装入口同源。
pub fn verify(tmap_bytes: &[u8]) -> Result<crate::VerifiedPlugin, tma_plugin_sdk::PackageError> {
    tma_plugin_sdk::verify(tmap_bytes, &[test_verifying_key()])
}

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
}
