#!/usr/bin/env sh
# 对 plugins/<name> 做签名打包：
#   target 里的 wasm 复制为插件目录下 plugin.wasm（中间产物，打完即删）
#   → tma-plugin-dev pack → dist/<name>.tmap + dist/<name>.tmap.sha256
# （dist/ 为 git 忽略产物，供 Release 上传与 update-catalog.py 读取）
#
# 前置：在插件目录内完成
#   cargo build --target wasm32-unknown-unknown --release
# （各插件目录是独立 workspace，target/ 落在插件目录下）
#
# 用法：scripts/build-package.sh plugins/<name>
# 环境：
#   TMA_PLUGIN_SIGNING_KEY_B64  base64 ed25519 发布私钥（必填；CI secret 注入，绝不入库）
#   TMA_PLUGIN_DEV              tma-plugin-dev 命令（可含参数，如
#                               "cargo run -p tma-plugin-dev --"；缺省 PATH 中的
#                               tma-plugin-dev；覆盖的命令须兼容 0.3 CLI——
#                               私钥走 env / --key-file，不接受位置参数）
set -eu

: "${TMA_PLUGIN_SIGNING_KEY_B64:?set TMA_PLUGIN_SIGNING_KEY_B64 to a release-only Ed25519 seed}"
TMA_PLUGIN_DEV="${TMA_PLUGIN_DEV:-tma-plugin-dev}"

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "${1:?用法: scripts/build-package.sh plugins/<name>}" in
  /*) dir="${1%/}" ;;
  *)  dir="$root/${1%/}" ;;
esac
name=$(basename "$dir")
[ -f "$dir/manifest.json" ] || {
  echo "缺 $dir/manifest.json（参数应为 plugins/<name> 插件目录）" >&2
  exit 1
}

# wasm 产物名 = crate 名连字符转下划线；crate 名取 Cargo.toml 首个
# `name = "..."`（[package] 段先于其他段出现）。
crate=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$dir/Cargo.toml" | head -n 1)
[ -n "$crate" ] || { echo "无法从 $dir/Cargo.toml 解析 package name" >&2; exit 1; }
wasm="$dir/target/wasm32-unknown-unknown/release/$(printf '%s' "$crate" | tr '-' '_').wasm"
[ -f "$wasm" ] || {
  echo "missing $wasm; run cargo build --target wasm32-unknown-unknown --release first" >&2
  exit 1
}

mkdir -p "$dir/dist"
cp "$wasm" "$dir/plugin.wasm"
# tma-plugin-dev 0.3 CLI：私钥走 TMA_PLUGIN_SIGNING_KEY_B64（上文已必填校验），
# 不进 argv。$TMA_PLUGIN_DEV 刻意不加引号（允许 "cargo run -p … --" 形态）。
$TMA_PLUGIN_DEV pack "$dir" -o "$dir/dist/$name.tmap"
# sha256 与被校验文件同目录、记相对名，便于 Release 页直接对照。
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$dir/dist" && sha256sum "$name.tmap" > "$name.tmap.sha256")
else
  (cd "$dir/dist" && shasum -a 256 "$name.tmap" > "$name.tmap.sha256")
fi
rm -f "$dir/plugin.wasm"
echo "完成：$dir/dist/$name.tmap (+ .sha256)"
