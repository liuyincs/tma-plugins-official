//! `.tmap` 插件包格式：打包、ed25519 验签、清单校验。
//!
//! 包是普通 ZIP（STORED，无压缩依赖），固定三个条目 + 可选第四个：
//!
//! - `manifest.json`：清单原始字节（验签输入，**原样保留**，不做规范化往返）；
//! - `plugin.wasm`：wasm 模块字节；
//! - `SIGNATURE.sig`：64 字节 ed25519 detached 签名；
//! - `icon.svg` / `icon.png`（可选，二选一，至多一个）：插件图标，≤ 64KiB，
//!   后缀与内容须对应（svg→XML 起始，png→PNG 魔数）。
//!
//! 签名输入（ed25519 detached 的 message）：
//!
//! - 无 icon 条目（3 条目旧包，继续兼容验签）：
//!   `manifest.json` 原始字节 ‖ `plugin.wasm` 的 SHA-256 摘要字节；
//! - 有 icon 条目：上述输入再 ‖ icon 条目的 SHA-256 摘要字节
//!   （改任一条目都令旧签名失效）。
//!
//! 验证顺序：解包（含 icon 条目校验）→ 任一受信公钥验签通过 →
//! `manifest.validate()` → `abi_compatible(HOST_ABI)`。防篡改完全由签名保证，
//! 宿主不信任包内任何自述。

use std::io::{Read, Write};

use crate::{AbiRange, AbiVersion, HOST_ABI, ManifestError, PluginManifest};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use thiserror::Error;

/// `.tmap` 打包和验签错误。
#[derive(Debug, Error)]
pub enum PackageError {
    #[error("invalid tmap archive: {0}")]
    BadArchive(String),
    #[error("tmap is missing required entry: {0}")]
    MissingEntry(&'static str),
    #[error("signature verification failed: {0}")]
    Signature(String),
    #[error("invalid manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("plugin abi {plugin:?} does not contain host abi {host:?}")]
    AbiIncompatible { plugin: AbiRange, host: AbiVersion },
    #[error("invalid plugin icon entry: {0}")]
    InvalidIcon(String),
}

const MANIFEST_ENTRY: &str = "manifest.json";
const WASM_ENTRY: &str = "plugin.wasm";
const SIGNATURE_ENTRY: &str = "SIGNATURE.sig";
const ICON_SVG_ENTRY: &str = "icon.svg";
const ICON_PNG_ENTRY: &str = "icon.png";

/// icon 条目字节数上限（64KiB：插件图标绰绰有余，挡失控资源）。
const MAX_ICON_BYTES: usize = 65_536;

/// PNG 魔数（8 字节）。
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// 包内 icon 条目（可选第 4 条目的解码形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginIcon {
    /// 条目名（`icon.svg` / `icon.png`，内容类型由此确定）。
    pub name: &'static str,
    /// 图标原始字节（签名覆盖其 SHA-256）。
    pub bytes: Vec<u8>,
}

impl PluginIcon {
    /// 内容类型（svg→`image/svg+xml`，png→`image/png`）。
    pub fn content_type(&self) -> &'static str {
        match self.name {
            ICON_SVG_ENTRY => "image/svg+xml",
            _ => "image/png",
        }
    }
}

/// 打包写入用固定时间戳（1980-01-01，zip 纪元起点）：同输入 → 同字节输出，
/// 固定测试密钥重打包可复现。
fn pack_timestamp() -> zip::DateTime {
    zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .expect("1980-01-01 00:00:00 是合法 zip 时间戳")
}

/// 签名输入（无 icon 条目）：`manifest.json` 原始字节 ‖ `plugin.wasm` 的 SHA-256 字节。
fn signing_input(manifest_bytes: &[u8], wasm_bytes: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(wasm_bytes);
    let mut input = Vec::with_capacity(manifest_bytes.len() + digest.len());
    input.extend_from_slice(manifest_bytes);
    input.extend_from_slice(&digest);
    input
}

/// 签名输入（有 icon 条目）：无 icon 输入再 ‖ icon 字节的 SHA-256。
fn signing_input_with_icon(manifest_bytes: &[u8], wasm_bytes: &[u8], icon_bytes: &[u8]) -> Vec<u8> {
    let mut input = signing_input(manifest_bytes, wasm_bytes);
    input.extend_from_slice(&Sha256::digest(icon_bytes));
    input
}

/// icon 条目校验：大小上限 + 后缀与内容对应（svg→XML 起始，png→PNG 魔数）。
///
/// SVG 是文本格式无魔数，只做宽松起始校验（剥 BOM/空白后以 `<` 开头，
/// 兼容 XML prolog 与 `<svg` 起始两种写法）；深度解析留给消费端。
fn validate_icon(name: &str, bytes: &[u8]) -> Result<(), PackageError> {
    if bytes.is_empty() {
        return Err(PackageError::InvalidIcon(format!("条目 {name} 内容为空")));
    }
    if bytes.len() > MAX_ICON_BYTES {
        return Err(PackageError::InvalidIcon(format!(
            "条目 {name} 为 {} 字节，超过上限 {MAX_ICON_BYTES}",
            bytes.len()
        )));
    }
    match name {
        ICON_SVG_ENTRY => {
            let body = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
            let trimmed = body
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map(|i| &body[i..])
                .unwrap_or(body);
            if !trimmed.starts_with(b"<") {
                return Err(PackageError::InvalidIcon(
                    "icon.svg 内容不是 SVG/XML（应以为 < 起始的 image/svg+xml）".into(),
                ));
            }
        }
        ICON_PNG_ENTRY => {
            if !bytes.starts_with(&PNG_MAGIC) {
                return Err(PackageError::InvalidIcon(
                    "icon.png 内容缺少 PNG 魔数（非 image/png）".into(),
                ));
            }
        }
        other => {
            return Err(PackageError::InvalidIcon(format!(
                "icon 条目名只允许 {ICON_SVG_ENTRY} 或 {ICON_PNG_ENTRY}，收到 {other:?}"
            )));
        }
    }
    Ok(())
}

/// 把 manifest + wasm 用给定私钥签名并打成 `.tmap` 字节（3 条目，无 icon）。
pub fn pack(
    manifest_bytes: &[u8],
    wasm_bytes: &[u8],
    signing_key: &SigningKey,
) -> Result<Vec<u8>, PackageError> {
    let signature = signing_key.sign(&signing_input(manifest_bytes, wasm_bytes));
    write_zip(
        manifest_bytes,
        wasm_bytes,
        &signature.to_bytes(),
        pack_timestamp(),
        None,
    )
}

/// 把 manifest + wasm + icon 用给定私钥签名并打成 `.tmap` 字节（4 条目）。
///
/// icon 先过 [`validate_icon`]（不签坏包），签名输入含 icon 哈希（见模块注释）。
pub fn pack_with_icon(
    manifest_bytes: &[u8],
    wasm_bytes: &[u8],
    icon: &PluginIcon,
    signing_key: &SigningKey,
) -> Result<Vec<u8>, PackageError> {
    validate_icon(icon.name, &icon.bytes)?;
    let signature = signing_key.sign(&signing_input_with_icon(
        manifest_bytes,
        wasm_bytes,
        &icon.bytes,
    ));
    write_zip(
        manifest_bytes,
        wasm_bytes,
        &signature.to_bytes(),
        pack_timestamp(),
        Some(icon),
    )
}

/// 按固定格式写 zip（`pack` / `pack_with_icon` 与测试伪造共用）。
fn write_zip(
    manifest_bytes: &[u8],
    wasm_bytes: &[u8],
    signature_bytes: &[u8],
    timestamp: zip::DateTime,
    icon: Option<&PluginIcon>,
) -> Result<Vec<u8>, PackageError> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buf);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(timestamp);

    let entries: [(&str, &[u8]); 3] = [
        (MANIFEST_ENTRY, manifest_bytes),
        (WASM_ENTRY, wasm_bytes),
        (SIGNATURE_ENTRY, signature_bytes),
    ];
    for (name, bytes) in entries {
        writer.start_file(name, options).map_err(zip_err)?;
        writer
            .write_all(bytes)
            .map_err(|e| PackageError::BadArchive(format!("写入条目 {name} 失败: {e}")))?;
    }
    if let Some(icon) = icon {
        writer.start_file(icon.name, options).map_err(zip_err)?;
        writer
            .write_all(&icon.bytes)
            .map_err(|e| PackageError::BadArchive(format!("写入条目 {} 失败: {e}", icon.name)))?;
    }
    let cursor = writer
        .finish()
        .map_err(|e| PackageError::BadArchive(format!("收尾 zip 失败: {e}")))?;
    Ok(cursor.into_inner())
}

/// zip 错误统一收口（写入路径）。
fn zip_err(e: zip::result::ZipError) -> PackageError {
    PackageError::BadArchive(e.to_string())
}

/// 验签通过的插件包（manifest 已解析校验，wasm 字节原样可用）。
#[derive(Debug, Clone)]
pub struct VerifiedPlugin {
    pub manifest: PluginManifest,
    /// 包内 `manifest.json` 原始字节（探测比对与审计用）。
    pub manifest_bytes: Vec<u8>,
    pub wasm: Vec<u8>,
    /// 可选 icon 条目（无该条目为 `None`；有则已过大小/内容校验且被签名覆盖）。
    pub icon: Option<PluginIcon>,
}

/// 验证 `.tmap` 字节：解包（含 icon 校验）→ 任一受信公钥验签 → 清单校验 → ABI 兼容。
///
/// 不受信公钥 / 篡改包（含 icon）/ 缺文件 / ABI 越界各自给出可区分的错误。
pub fn verify(bytes: &[u8], trusted_keys: &[VerifyingKey]) -> Result<VerifiedPlugin, PackageError> {
    let (manifest_bytes, wasm_bytes, signature_bytes, icon) = unpack(bytes)?;

    if trusted_keys.is_empty() {
        return Err(PackageError::Signature(
            "no trusted keys configured (default deny)".into(),
        ));
    }
    let message = match &icon {
        Some(icon) => signing_input_with_icon(&manifest_bytes, &wasm_bytes, &icon.bytes),
        None => signing_input(&manifest_bytes, &wasm_bytes),
    };
    let signature = Signature::from_slice(&signature_bytes).map_err(|e| {
        PackageError::Signature(format!("签名长度/格式非法（应为 64 字节 ed25519）: {e}"))
    })?;
    let ok = trusted_keys
        .iter()
        .any(|key| key.verify(&message, &signature).is_ok());
    if !ok {
        return Err(PackageError::Signature(
            "signature does not match any trusted key".into(),
        ));
    }

    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        PackageError::BadArchive(format!("{MANIFEST_ENTRY} 不是合法清单 JSON: {e}"))
    })?;
    manifest.validate()?;
    if !manifest.abi_compatible(HOST_ABI) {
        return Err(PackageError::AbiIncompatible {
            plugin: manifest.abi,
            host: HOST_ABI,
        });
    }

    Ok(VerifiedPlugin {
        manifest,
        manifest_bytes,
        wasm: wasm_bytes,
        icon,
    })
}

/// 解出三个必要条目 + 可选 icon 条目（多余条目忽略，缺必要条目即错）。
/// 解包产物：manifest 原始字节 / wasm 字节 / 签名字节 / icon（可选）。
type Unpacked = (Vec<u8>, Vec<u8>, Vec<u8>, Option<PluginIcon>);

fn unpack(bytes: &[u8]) -> Result<Unpacked, PackageError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| PackageError::BadArchive(e.to_string()))?;
    let manifest = read_entry(&mut archive, MANIFEST_ENTRY)?;
    let wasm = read_entry(&mut archive, WASM_ENTRY)?;
    let signature = read_entry(&mut archive, SIGNATURE_ENTRY)?;
    let icon = read_icon_entry(&mut archive)?;
    Ok((manifest, wasm, signature, icon))
}

/// 读取可选 icon 条目：`icon.svg`/`icon.png` 二选一；并存 / 超限 / 内容与
/// 后缀不符给出可区分的 [`PackageError::InvalidIcon`]。
fn read_icon_entry(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
) -> Result<Option<PluginIcon>, PackageError> {
    let has_svg = archive.file_names().any(|n| n == ICON_SVG_ENTRY);
    let has_png = archive.file_names().any(|n| n == ICON_PNG_ENTRY);
    if has_svg && has_png {
        return Err(PackageError::InvalidIcon(
            "icon.svg 与 icon.png 至多一个".into(),
        ));
    }
    let name = if has_svg {
        ICON_SVG_ENTRY
    } else if has_png {
        ICON_PNG_ENTRY
    } else {
        return Ok(None);
    };
    let bytes = read_entry(archive, name)?;
    validate_icon(name, &bytes)?;
    Ok(Some(PluginIcon { name, bytes }))
}

fn read_entry(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &'static str,
) -> Result<Vec<u8>, PackageError> {
    let mut file = archive.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => PackageError::MissingEntry(name),
        other => PackageError::BadArchive(format!("读取条目 {name} 失败: {other}")),
    })?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| PackageError::BadArchive(format!("读取条目 {name} 失败: {e}")))?;
    Ok(buf)
}

// wasm32 不跑打包单测：dev-dep `rand` 不提供 wasm32-unknown-unknown 的 OsRng
// （见 Cargo.toml 的 target 限定说明）。
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn test_manifest_bytes() -> Vec<u8> {
        r#"{
            "id": "tma.test.echo",
            "name": "Echo",
            "version": "0.1.0",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 6 } },
            "extension_points": ["scrape_provider"],
            "permissions": [
                {"http": {"scheme": "http", "host": "127.0.0.1", "reason": "本地回环测试"}}
            ],
            "scrape": {
                "provider": "echo_images",
                "capabilities": ["image"],
                "requires_credentials": false
            }
        }"#
        .as_bytes()
        .to_vec()
    }

    /// 最小合法 wasm 模块（`\0asm` + 版本 1）。
    fn minimal_wasm() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    /// 最小合法 SVG（XML 起始）与 PNG（魔数 + 任意负载）字节。
    fn minimal_svg() -> Vec<u8> {
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"/>"#.to_vec()
    }

    fn minimal_png() -> Vec<u8> {
        // 硬编码真实 PNG 签名（不从 PNG_MAGIC 派生，防止常量写错时测试自洽通过）。
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(b"IEND-payload");
        bytes
    }

    #[test]
    fn pack_verify_roundtrip() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest = test_manifest_bytes();
        let wasm = minimal_wasm();
        let packed = pack(&manifest, &wasm, &key).unwrap();
        // 确定性：同输入重打包字节一致。
        assert_eq!(pack(&manifest, &wasm, &key).unwrap(), packed);

        let verified = verify(&packed, &[key.verifying_key()]).unwrap();
        assert_eq!(verified.manifest_bytes, manifest);
        assert_eq!(verified.wasm, wasm);
        assert_eq!(verified.manifest.id, "tma.test.echo");
    }

    /// 3 条目旧包回归：无 icon 的包形态不变，继续通过验签（icon 分支不碰旧输入）。
    #[test]
    fn legacy_three_entry_pack_still_verifies() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest = test_manifest_bytes();
        let wasm = minimal_wasm();
        let packed = pack(&manifest, &wasm, &key).unwrap();

        // 条目恰为固定三条（无 icon 条目混入）。
        let cursor = std::io::Cursor::new(&packed);
        let archive = zip::ZipArchive::new(cursor).expect("pack 产物必须是合法 zip");
        let names: Vec<&str> = archive.file_names().collect();
        assert_eq!(names, [MANIFEST_ENTRY, WASM_ENTRY, SIGNATURE_ENTRY]);

        let verified = verify(&packed, &[key.verifying_key()]).unwrap();
        assert!(verified.icon.is_none());
    }

    /// icon 条目往返：svg/png 两种形态 pack → verify，签名输入覆盖 icon 哈希。
    #[test]
    fn icon_entry_pack_verify_roundtrip() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest = test_manifest_bytes();
        let wasm = minimal_wasm();

        for icon in [
            PluginIcon {
                name: ICON_SVG_ENTRY,
                bytes: minimal_svg(),
            },
            PluginIcon {
                name: ICON_PNG_ENTRY,
                bytes: minimal_png(),
            },
        ] {
            let packed = pack_with_icon(&manifest, &wasm, &icon, &key).unwrap();
            // 确定性：同输入重打包字节一致。
            assert_eq!(
                pack_with_icon(&manifest, &wasm, &icon, &key).unwrap(),
                packed
            );
            let verified = verify(&packed, &[key.verifying_key()]).unwrap();
            let got = verified.icon.as_ref().expect("icon 条目应解出");
            assert_eq!(got.name, icon.name);
            assert_eq!(got.bytes, icon.bytes);
            assert_eq!(got.content_type(), icon.content_type());
        }
    }

    /// 篡改 icon（内容合法但字节不同）沿用旧签名 → 验签失败（icon 哈希在签名输入内）。
    #[test]
    fn tampered_icon_is_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest = test_manifest_bytes();
        let wasm = minimal_wasm();
        let icon = PluginIcon {
            name: ICON_PNG_ENTRY,
            bytes: minimal_png(),
        };
        let stale_sig = key.sign(&signing_input_with_icon(&manifest, &wasm, &icon.bytes));

        let mut other = minimal_png();
        other.extend_from_slice(b"tamper"); // 仍是合法 PNG 形态
        let forged = write_zip(
            &manifest,
            &wasm,
            &stale_sig.to_bytes(),
            pack_timestamp(),
            Some(&PluginIcon {
                name: ICON_PNG_ENTRY,
                bytes: other,
            }),
        )
        .unwrap();
        assert!(matches!(
            verify(&forged, &[key.verifying_key()]),
            Err(PackageError::Signature(_))
        ));

        // 换成 svg 也同理（条目名变化即内容变化，签名输入对不上）。
        let forged = write_zip(
            &manifest,
            &wasm,
            &stale_sig.to_bytes(),
            pack_timestamp(),
            Some(&PluginIcon {
                name: ICON_SVG_ENTRY,
                bytes: minimal_svg(),
            }),
        )
        .unwrap();
        assert!(matches!(
            verify(&forged, &[key.verifying_key()]),
            Err(PackageError::Signature(_))
        ));
    }

    /// icon 校验拒绝侧：超限 / 空 / 后缀与内容不符 / 条目名非法 / 两者并存。
    #[test]
    fn invalid_icon_entries_are_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest = test_manifest_bytes();
        let wasm = minimal_wasm();

        // pack 侧：超限与错误内容直接拒绝（不值得签坏包）。
        let oversized = PluginIcon {
            name: ICON_SVG_ENTRY,
            bytes: vec![b'<'; MAX_ICON_BYTES + 1],
        };
        assert!(matches!(
            pack_with_icon(&manifest, &wasm, &oversized, &key),
            Err(PackageError::InvalidIcon(_))
        ));
        let svg_with_png_magic = PluginIcon {
            name: ICON_SVG_ENTRY,
            bytes: minimal_png(),
        };
        assert!(matches!(
            pack_with_icon(&manifest, &wasm, &svg_with_png_magic, &key),
            Err(PackageError::InvalidIcon(_))
        ));
        let png_with_svg_body = PluginIcon {
            name: ICON_PNG_ENTRY,
            bytes: minimal_svg(),
        };
        assert!(matches!(
            pack_with_icon(&manifest, &wasm, &png_with_svg_body, &key),
            Err(PackageError::InvalidIcon(_))
        ));

        // verify 侧：手工构造坏 icon 条目（签名按其真实字节算，仍过不了结构关）。
        let mut oversized_bytes = PNG_MAGIC.to_vec();
        oversized_bytes.extend(std::iter::repeat_n(0u8, MAX_ICON_BYTES + 1));
        let oversized = PluginIcon {
            name: ICON_PNG_ENTRY,
            bytes: oversized_bytes,
        };
        let sig = key.sign(&signing_input_with_icon(&manifest, &wasm, &oversized.bytes));
        let packed = write_zip(
            &manifest,
            &wasm,
            &sig.to_bytes(),
            pack_timestamp(),
            Some(&oversized),
        )
        .unwrap();
        assert!(matches!(
            verify(&packed, &[key.verifying_key()]),
            Err(PackageError::InvalidIcon(_))
        ));

        // 两者并存：即便各自合法、签名覆盖其一，也直接拒绝（包格式至多一个 icon）。
        let svg = PluginIcon {
            name: ICON_SVG_ENTRY,
            bytes: minimal_svg(),
        };
        let png = PluginIcon {
            name: ICON_PNG_ENTRY,
            bytes: minimal_png(),
        };
        let sig = key.sign(&signing_input_with_icon(&manifest, &wasm, &png.bytes));
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .last_modified_time(pack_timestamp());
            for (name, bytes) in [
                (MANIFEST_ENTRY, manifest.as_slice()),
                (WASM_ENTRY, wasm.as_slice()),
                (SIGNATURE_ENTRY, sig.to_bytes().as_slice()),
                (ICON_PNG_ENTRY, png.bytes.as_slice()),
                (ICON_SVG_ENTRY, svg.bytes.as_slice()),
            ] {
                writer.start_file(name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        assert!(matches!(
            verify(buf.get_ref(), &[key.verifying_key()]),
            Err(PackageError::InvalidIcon(_))
        ));
    }

    #[test]
    fn tampering_any_entry_is_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let manifest1 = test_manifest_bytes();
        let manifest2 = {
            // 签名校验先于 JSON 解析，内容合法性无关紧要。
            let mut m = manifest1.clone();
            m.push(b' ');
            m
        };
        let wasm1 = minimal_wasm();
        // 与 wasm1 内容不同（自定义段名占位字节），模拟"替换 wasm 沿用旧签名"。
        let wasm2 = {
            let mut w = minimal_wasm();
            w.extend_from_slice(b"tamper");
            w
        };
        let trusted = [key.verifying_key()];

        // 攻击者替换 manifest 但沿用旧签名：签名输入对不上 → 拒绝。
        let stale_sig = key.sign(&signing_input(&manifest1, &wasm1));
        let forged = pack_raw(&manifest2, &wasm1, &stale_sig.to_bytes());
        assert!(matches!(
            verify(&forged, &trusted),
            Err(PackageError::Signature(_))
        ));

        // 攻击者替换 wasm 但沿用旧签名。
        let forged = pack_raw(&manifest1, &wasm2, &stale_sig.to_bytes());
        assert!(matches!(
            verify(&forged, &trusted),
            Err(PackageError::Signature(_))
        ));

        // 随机 64 字节当签名。
        let forged = pack_raw(&manifest1, &wasm1, &[0u8; 64]);
        assert!(matches!(
            verify(&forged, &trusted),
            Err(PackageError::Signature(_))
        ));

        // 坏 zip 字节（非 zip）。
        assert!(matches!(
            verify(b"not a zip at all", &trusted),
            Err(PackageError::BadArchive(_))
        ));
    }

    /// 测试专用：按固定格式写任意三段字节（绕过 pack 的签名计算，模拟重打包攻击）。
    fn pack_raw(manifest: &[u8], wasm: &[u8], signature: &[u8]) -> Vec<u8> {
        write_zip(manifest, wasm, signature, pack_timestamp(), None).expect("测试内打包不应失败")
    }

    #[test]
    fn untrusted_key_is_rejected() {
        let signer = SigningKey::generate(&mut OsRng);
        let other = SigningKey::generate(&mut OsRng);
        let packed = pack(&test_manifest_bytes(), &minimal_wasm(), &signer).unwrap();
        assert!(matches!(
            verify(&packed, &[other.verifying_key()]),
            Err(PackageError::Signature(_))
        ));
        // 空受信集合 = 默认拒绝。
        assert!(matches!(
            verify(&packed, &[]),
            Err(PackageError::Signature(_))
        ));
    }

    #[test]
    fn abi_out_of_range_is_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let old_manifest = br#"{
            "id": "tma.test.old",
            "name": "Old",
            "version": "0.1.0",
            "abi": { "min": { "major": 1, "minor": 0 }, "max": { "major": 1, "minor": 0 } },
            "extension_points": ["scrape_provider"],
            "scrape": {"provider": "old", "capabilities": ["image"], "requires_credentials": false}
        }"#;
        let packed = pack(old_manifest, &minimal_wasm(), &key).unwrap();
        match verify(&packed, &[key.verifying_key()]) {
            Err(PackageError::AbiIncompatible { plugin, host }) => {
                assert_eq!(host, HOST_ABI);
                assert!(!plugin.contains(host));
            }
            other => panic!("期望 AbiIncompatible，实际 {other:?}"),
        }
    }

    #[test]
    fn garbage_input_is_rejected_without_panic() {
        let key = SigningKey::generate(&mut OsRng);
        assert!(matches!(
            verify(b"", &[key.verifying_key()]),
            Err(PackageError::BadArchive(_))
        ));
        // 空字节也是坏包（截断/空文件）。
        assert!(matches!(
            verify(&[0u8; 4], &[key.verifying_key()]),
            Err(PackageError::BadArchive(_))
        ));
    }
}
