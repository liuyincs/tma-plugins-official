#!/usr/bin/env python3
"""把单个插件的发布条目 upsert 进仓库根 catalog.json。

用法：
    scripts/update-catalog.py plugins/<name> [--version X.Y.Z] [--tag TAG]
                              [--download-url URL] [--public-key-url URL]
                              [--repo-slug OWNER/REPO]

数据来源：
- plugins/<name>/manifest.json：id/name/description/author/homepage/abi/permissions；
- plugins/<name>/dist/<name>.tmap.sha256：build-package.sh 的产物校验和
  （若 dist/<name>.tmap 同在旁边，顺手复核 sha 一致，防陈旧校验和入库）；
- 仓库根 public-key.txt：base64 ed25519 官方签名公钥，fingerprint =
  "sha256:" + sha256hex(base64decode(文件内容))。

缺省推导（与 .github/workflows/release.yml 约定一致）：
- version：manifest.json 的 version；
- tag：<name>-v<version>（<name> 为 plugins/ 下目录名）；
- download_url：https://github.com/<repo>/releases/download/<tag>/<name>.tmap；
- public_key_url：https://raw.githubusercontent.com/<repo>/<tag>/public-key.txt；
- <repo>：origin remote 的 github.com slug，解析失败回落 DEFAULT_REPO。
"""

import argparse
import base64
import hashlib
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_REPO = "liuyincs/tma-plugins-official"
SEMVER_RE = re.compile(
    r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"
)


def repo_slug() -> str:
    try:
        url = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except Exception:
        return DEFAULT_REPO
    m = re.search(r"github\.com[:/]([^/]+)/([^/]+?)(?:\.git)?$", url)
    return f"{m.group(1)}/{m.group(2)}" if m else DEFAULT_REPO


def die(msg: str) -> "SystemExit":
    return SystemExit(f"error: {msg}")


def main() -> None:
    ap = argparse.ArgumentParser(description="upsert 插件条目进 catalog.json")
    ap.add_argument("plugin_dir", help="plugins/<name> 插件目录")
    ap.add_argument("--version", help="发布版本（缺省取 manifest.json 的 version）")
    ap.add_argument("--tag", help="release tag（缺省 <name>-v<version>）")
    ap.add_argument("--download-url", help="覆盖 .tmap 下载地址")
    ap.add_argument("--public-key-url", help="覆盖公钥文件地址")
    ap.add_argument("--repo-slug", help="覆盖 github.com 的 owner/repo")
    args = ap.parse_args()

    plugin_dir = (ROOT / args.plugin_dir).resolve()
    name = plugin_dir.name

    manifest_path = plugin_dir / "manifest.json"
    if not manifest_path.is_file():
        raise die(f"缺 {manifest_path}（参数应为 plugins/<name> 插件目录）")
    manifest = json.loads(manifest_path.read_text())
    for key in ("id", "version", "abi"):
        if key not in manifest:
            raise die(f"{manifest_path} 缺必填字段 {key}")

    version = args.version or manifest["version"]
    if not SEMVER_RE.match(version):
        raise die(f"version {version!r} 不是合法 SemVer")
    tag = args.tag or f"{name}-v{version}"
    slug = args.repo_slug or repo_slug()
    download_url = args.download_url or (
        f"https://github.com/{slug}/releases/download/{tag}/{name}.tmap"
    )
    public_key_url = args.public_key_url or (
        f"https://raw.githubusercontent.com/{slug}/{tag}/public-key.txt"
    )

    sha_file = plugin_dir / "dist" / f"{name}.tmap.sha256"
    if not sha_file.is_file():
        raise die(f"缺 {sha_file}（先跑 scripts/build-package.sh {args.plugin_dir}）")
    sha = sha_file.read_text().split()[0].strip()
    if len(sha) != 64 or any(c not in "0123456789abcdefABCDEF" for c in sha):
        raise die(f"非法 SHA-256：{sha!r}")
    tmap = plugin_dir / "dist" / f"{name}.tmap"
    if tmap.is_file():
        actual = hashlib.sha256(tmap.read_bytes()).hexdigest()
        if actual != sha.lower():
            raise die(f"{tmap} 实际 sha256 与 {sha_file} 不一致（产物陈旧？）")

    pubkey_path = ROOT / "public-key.txt"
    if not pubkey_path.is_file():
        raise die(
            f"缺 {pubkey_path}（官方签名公钥，tma-plugin-dev pubkey 导出后提交）"
        )
    try:
        pubkey_bytes = base64.b64decode(
            "".join(pubkey_path.read_text().split()), validate=True
        )
    except Exception as e:
        raise die(f"{pubkey_path} 不是合法 base64：{e}")
    fingerprint = f"sha256:{hashlib.sha256(pubkey_bytes).hexdigest()}"

    # 字段顺序与既有 catalog 样例一致；manifest 未声明的可选键不占位。
    entry = {
        "id": manifest["id"],
        "name": manifest.get("name"),
        "version": version,
        "description": manifest.get("description"),
        "author": manifest.get("author"),
        "homepage": manifest.get("homepage")
        or f"https://github.com/{slug}/tree/main/plugins/{name}",
        "abi": manifest["abi"],
        "permissions": manifest.get("permissions", []),
        "download_url": download_url,
        "sha256": sha,
        "public_key_url": public_key_url,
        "public_key_fingerprint": fingerprint,
    }
    entry = {k: v for k, v in entry.items() if v is not None}

    catalog_path = ROOT / "catalog.json"
    catalog = json.loads(catalog_path.read_text())
    plugins = catalog.setdefault("plugins", [])
    for i, existing in enumerate(plugins):
        if existing.get("id") == entry["id"]:
            plugins[i] = entry
            verb = "updated"
            break
    else:
        plugins.append(entry)
        verb = "added"
    catalog_path.write_text(
        json.dumps(catalog, ensure_ascii=False, indent=2) + "\n"
    )
    print(f"catalog.json: {verb} {entry['id']} {version} (tag {tag})")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - 脚本化失败打印一行即可
        print(f"error: {e}", file=sys.stderr)
        raise SystemExit(1)
