#!/usr/bin/env python3
"""扫描 plugins/ 一级子目录发现插件并做契约校验，输出 CI matrix JSON。

用法：
    scripts/discover-plugins.py [--root <仓库根>]

契约（.github/workflows/test.yml 的 discover job 消费本脚本输出）：
- 扫描 <root>/plugins/ 的一级、非隐藏子目录（`.` 开头跳过）；symlink
  一律失败（插件目录必须是真实目录）；plugins/ 不存在或扫不到任何
  插件目录即失败；任一目录不合规整个发现失败；
- 目录名须为单个安全路径段（[A-Za-z0-9_-]+），拒绝 `./`/`..`/`$(` 等
  会改变路径或触发 shell 展开的名字；
- 每个插件目录必须同时含 manifest.json 与 Cargo.toml；
- manifest.json 必须是合法 JSON object，且 id/name/version 均为非空
  字符串；id 还须满足 docs/authoring.md 的规则：[A-Za-z0-9_] 按点
  分段、禁空段、≤200 字符；
- 全部插件 id 全局唯一。

全部通过后向 stdout 打印单行 {"dir":["<目录名>",...]}（目录名字典序，
输出稳定），供 workflow 里 `matrix: ${{ fromJSON(...) }}` 直接消费。
错误一律写 stderr 并以非零退出。

--root 便于用临时 fixture 目录验证失败路径；缺省为脚本推导的仓库根。
"""

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ID_RE = re.compile(r"^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*$")
ID_MAX_LEN = 200
DIR_RE = re.compile(r"^[A-Za-z0-9_-]+$")


def die(msg: str) -> "SystemExit":
    return SystemExit(f"error: {msg}")


def check_manifest(plugin_dir: pathlib.Path) -> str:
    manifest_path = plugin_dir / "manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except Exception as e:
        raise die(f"插件目录 {plugin_dir.name} 的 manifest.json 不是合法 JSON：{e}")
    if not isinstance(manifest, dict):
        raise die(f"插件目录 {plugin_dir.name} 的 manifest.json 不是 JSON object")
    for key in ("id", "name", "version"):
        value = manifest.get(key)
        if not isinstance(value, str) or not value.strip():
            raise die(
                f"插件目录 {plugin_dir.name} 的 manifest.json 字段 {key} "
                "缺失或不是非空字符串"
            )
    plugin_id = manifest["id"]
    if len(plugin_id) > ID_MAX_LEN or not ID_RE.fullmatch(plugin_id):
        raise die(
            f"插件目录 {plugin_dir.name} 的 id {plugin_id!r} 非法"
            "（要求 [A-Za-z0-9_] 按点分段、禁空段、≤200 字符）"
        )
    return plugin_id


def discover(plugins_dir: pathlib.Path) -> list:
    if not plugins_dir.is_dir():
        raise die(f"缺插件根目录 {plugins_dir}")
    dirs = []
    for entry in sorted(plugins_dir.iterdir()):
        if entry.name.startswith("."):
            continue
        if entry.is_symlink():
            raise die(f"{entry} 是符号链接，插件目录必须是真实目录")
        if entry.is_dir():
            dirs.append(entry)
    if not dirs:
        raise die(f"{plugins_dir} 下没有任何插件目录")

    seen = {}  # plugin id -> 目录名
    names = []
    for plugin_dir in dirs:
        if not DIR_RE.fullmatch(plugin_dir.name):
            raise die(
                f"插件目录名 {plugin_dir.name!r} 非法"
                "（要求 [A-Za-z0-9_-]+，单个路径段）"
            )
        for filename in ("manifest.json", "Cargo.toml"):
            if not (plugin_dir / filename).is_file():
                raise die(f"插件目录 {plugin_dir.name} 缺 {filename}")
        plugin_id = check_manifest(plugin_dir)
        if plugin_id in seen:
            raise die(
                f"插件 id {plugin_id!r} 重复：目录 {seen[plugin_id]} 与 "
                f"{plugin_dir.name}"
            )
        seen[plugin_id] = plugin_dir.name
        names.append(plugin_dir.name)
    return names


def main() -> None:
    ap = argparse.ArgumentParser(
        description="发现 plugins/ 下插件并输出 CI matrix JSON"
    )
    ap.add_argument("--root", help="仓库根（缺省为脚本所在仓库）")
    args = ap.parse_args()
    root = pathlib.Path(args.root).resolve() if args.root else ROOT
    names = discover(root / "plugins")
    print(json.dumps({"dir": names}, separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - 脚本化失败打印一行即可
        print(f"error: {e}", file=sys.stderr)
        raise SystemExit(1)
