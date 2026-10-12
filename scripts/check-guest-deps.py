#!/usr/bin/env python3
"""校验各 workspace 实际解析的 guest 编解码依赖边一致性。

用法：
    scripts/check-guest-deps.py [--root <仓库根>]

背景（INT-6/46b22d2 漏网，29d80df 补齐）：testkit 升级宿主侧 extism 后，
Cargo 在插件 workspace 内对同 semver 区间统一解析，把 extism-pdk 的
extism-convert 一并抬升；模板 workspace 无此 dev-dep，lockfile 停在旧版，
guest 序列化闭包与生产插件漂移。此类漂移无法靠手工拆分 lockfile 规避，
只能靠检查守住。

比较规则（`.github/workflows/test.yml` 的 guest-deps job 消费本脚本）：

- 只跟踪 `extism-pdk -> extism-convert` 这一条 guest 边：convert 经 pdk
  编进 wasm 产物，属 guest 发布闭包；宿主侧 crate（testkit dev-dep 引入的
  extism/wasmtime 等）与 `tma-plugin-sdk` 等无关依赖的版本差异天然不参与
  比较——依赖升级检查分清 host dev-dep 与 guest 发布闭包。
- 按 pdk resolved 版本分组：同一 pdk 版本在所有 workspace 必须解析到同一
  convert 版本；不同 pdk 版本属不同版本线，不横向比较（不强制所有独立
  workspace 全部依赖一致）。单边分组（某 pdk 版本只出现于一个 workspace）
  无比较对象，仅报告。
- 偏差处置：不一致即失败；确需保留的差异在
  `scripts/guest-deps-exceptions.json` 逐边登记（每条必填 `reason`）：

      {"exceptions": [
        {"workspace": "plugins/foo", "extism_pdk": "1.4.1",
         "extism_convert": "1.30.0", "reason": "…"}
      ]}

  例外逐边生效：非例外边仍须全组一致；例外边锁死声明值，解析漂移、
  分组收敛（含全组例外收敛）或无比较对象时遗留的例外同样失败。
  例外按 (workspace, extism_pdk 版本) 命中：同 workspace 同版本多条
  边（不同 source）共用一条例外并各自校验声明值。

扫描面：`scripts/discover-plugins.py` 发现的生产插件 + `templates/` 下含
`Cargo.toml` 的一级子目录 + `testkit`。模板由本清单显式加入——生产插件
发现仍只覆盖 `plugins/`，模板不混入 manifest 目录清单。workspace 缺
`Cargo.lock` 直接失败（与 cargo `--locked` 同口径）。

`--root` 便于用临时 fixture 目录验证失败路径；缺省为脚本推导的仓库根。
只读检查：解析 lockfile 不运行 cargo，不修改工作树。
"""

import argparse
import json
import pathlib
import subprocess
import sys

try:
    import tomllib
except ImportError:  # Python < 3.11
    sys.exit("error: 需要 Python >= 3.11（tomllib 解析 Cargo.lock）")

ROOT = pathlib.Path(__file__).resolve().parents[1]
# (经由 crate, 目标 crate)：pdk 的 convert 是编进 wasm 的编解码闭包。
GUEST_EDGES = [("extism-pdk", "extism-convert")]
EXCEPTIONS_FILE = "scripts/guest-deps-exceptions.json"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def die(msg: str) -> "SystemExit":
    return SystemExit(f"error: {msg}")


def member_label(workspace: str, source: "str | None") -> str:
    """成员标签：crates.io 源只显示 workspace；path 包与 git/私有源
    分别追加 [path]、[source#前段] 区分同名同版本的不同来源边。"""
    if source == CRATES_IO:
        return workspace
    if source is None:
        return f"{workspace}[path]"
    return f"{workspace}[{source.split('#', 1)[0]}]"


def discover_plugins(root: pathlib.Path) -> "list[str]":
    script = root / "scripts" / "discover-plugins.py"
    proc = subprocess.run(
        [sys.executable, str(script), "--root", str(root)],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise die(f"插件发现失败：{proc.stderr.strip() or proc.stdout.strip()}")
    try:
        matrix = json.loads(proc.stdout)
        return [f"plugins/{d}" for d in matrix["dir"]]
    except Exception as e:
        raise die(f"discover-plugins.py 输出无法解析：{e}")


def find_workspaces(root: pathlib.Path) -> "list[str]":
    workspaces = discover_plugins(root)
    templates_dir = root / "templates"
    if templates_dir.is_dir():
        for entry in sorted(templates_dir.iterdir()):
            if entry.name.startswith("."):
                continue
            if entry.is_symlink():
                raise die(f"{entry} 是符号链接，模板目录必须是真实目录")
            if entry.is_dir() and (entry / "Cargo.toml").is_file():
                workspaces.append(f"templates/{entry.name}")
    workspaces.append("testkit")
    return workspaces


def parse_dep_spec(spec: str) -> "tuple[str, str | None, str | None]":
    """解析 Cargo.lock dependencies 条目：`name` / `name version` /
    `name version (source)`，返回 (name, version|None, source|None)。"""
    body, sep, tail = spec.partition(" (")
    source = tail[:-1] if sep and tail.endswith(")") else None
    name, _, version = body.partition(" ")
    return name, version or None, source


def resolve_dep_version(
    spec: str, by_name: "dict[str, list[dict]]", lock_path: pathlib.Path
) -> str:
    name, version, source = parse_dep_spec(spec)
    candidates = by_name.get(name, [])
    if version is not None:
        candidates = [p for p in candidates if p.get("version") == version]
    if source is not None:
        # dep spec 的 source 不带 #commit 后缀，包条目 source 带——按
        # '#'-前段比较（git+URL?query 部分仍区分不同 rev/branch）。
        want = source.split("#", 1)[0]
        candidates = [
            p
            for p in candidates
            if (p.get("source") or "").split("#", 1)[0] == want
        ]
    elif len(candidates) > 1:
        # `name version`（无 source）在同名同版本多候选时指向无 source 的
        # path/workspace 包——source 型候选的 spec 必带 (source) 标注。
        local = [p for p in candidates if p.get("source") is None]
        if len(local) == 1:
            candidates = local
    if len(candidates) != 1:
        raise die(
            f"{lock_path} 中依赖条目 {spec!r} 无法唯一定位"
            f"（命中 {len(candidates)} 个候选）"
        )
    return candidates[0]["version"]


def load_edges(root: pathlib.Path, workspace: str) -> "list[dict]":
    """返回该 workspace 的 guest 边列表，每条
    {via, via_version, via_source, target, target_version}；
    同一 (name,version) 不同 source 的包各占一条边，不覆盖。"""
    ws_dir = root / workspace
    if not (ws_dir / "Cargo.toml").is_file():
        raise die(f"workspace {workspace} 缺 Cargo.toml")
    lock_path = ws_dir / "Cargo.lock"
    if not lock_path.is_file():
        raise die(
            f"workspace {workspace} 缺 Cargo.lock——与 cargo --locked 同口径，"
            "先在本地生成并提交 lockfile"
        )
    try:
        data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    except Exception as e:
        raise die(f"{lock_path} 不是合法 TOML：{e}")
    if not isinstance(data.get("package"), list):
        raise die(f"{lock_path} 缺 [[package]] 段")

    by_name: "dict[str, list[dict]]" = {}
    for pkg in data["package"]:
        if isinstance(pkg, dict) and "name" in pkg:
            by_name.setdefault(pkg["name"], []).append(pkg)

    edges = []
    for via, target in GUEST_EDGES:
        for pkg in by_name.get(via, []):
            # 一个包可经 alias 依赖同一 crate 的多个版本——逐条依赖
            # 各成一条边，全部参与一致性比较。
            for spec in pkg.get("dependencies", []) or []:
                if parse_dep_spec(spec)[0] != target:
                    continue
                resolved = resolve_dep_version(spec, by_name, lock_path)
                edges.append(
                    {
                        "via": via,
                        "via_version": pkg["version"],
                        "via_source": pkg.get("source"),
                        "target": target,
                        "target_version": resolved,
                    }
                )
    return edges


def load_exceptions(root: pathlib.Path) -> "dict[tuple[str, str, str], dict]":
    """{(workspace, via, via_version): {convert, reason}}；文件缺省视为空。"""
    path = root / EXCEPTIONS_FILE
    if not path.is_file():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception as e:
        raise die(f"{path} 不是合法 JSON：{e}")
    items = data.get("exceptions") if isinstance(data, dict) else None
    if not isinstance(items, list):
        raise die(f"{path} 须为含 exceptions 数组的 JSON object")
    exceptions = {}
    for i, item in enumerate(items):
        if not isinstance(item, dict):
            raise die(f"{path} exceptions[{i}] 不是 object")
        entry = {}
        for key in ("workspace", "extism_pdk", "extism_convert", "reason"):
            value = item.get(key)
            if not isinstance(value, str) or not value.strip():
                raise die(f"{path} exceptions[{i}] 字段 {key} 缺失或为空")
            entry[key] = value
        key = (entry["workspace"], "extism-pdk", entry["extism_pdk"])
        if key in exceptions:
            raise die(f"{path} exceptions[{i}] 与前文 {key} 重复")
        exceptions[key] = entry
    return exceptions


def main() -> None:
    ap = argparse.ArgumentParser(
        description="校验各 workspace guest 编解码依赖边（pdk->convert）一致性"
    )
    ap.add_argument("--root", help="仓库根（缺省为脚本所在仓库）")
    args = ap.parse_args()
    root = pathlib.Path(args.root).resolve() if args.root else ROOT

    workspaces = find_workspaces(root)
    exceptions = load_exceptions(root)
    used_exceptions = set()

    # (via, via_version) -> [(workspace, via_source, target_version)]；
    # 同 workspace 内同名同版本不同 source 的包各占一条边，不合并覆盖。
    groups: "dict[tuple[str, str], list[tuple[str, str | None, str]]]" = {}
    for workspace in workspaces:
        for edge in load_edges(root, workspace):
            groups.setdefault((edge["via"], edge["via_version"]), []).append(
                (workspace, edge["via_source"], edge["target_version"])
            )

    errors = []
    for (via, via_version), edges in sorted(groups.items()):
        target_name = next(t for v, t in GUEST_EDGES if v == via)
        errors_before = len(errors)
        baseline = {}  # target_version -> [成员标签]（非例外边须一致）
        explained = []  # (成员标签, target_version, reason)
        for workspace, via_source, target_version in sorted(
            edges, key=lambda e: (e[0], e[1] or "", e[2])
        ):
            label = member_label(workspace, via_source)
            exc = exceptions.get((workspace, via, via_version))
            if exc is None:
                baseline.setdefault(target_version, []).append(label)
            elif exc["extism_convert"] == target_version:
                explained.append((label, target_version, exc["reason"]))
                used_exceptions.add((workspace, via, via_version))
            else:
                errors.append(
                    f"例外失效：{workspace} 声明 {via}@{via_version} -> "
                    f"{target_name} {exc['extism_convert']}，实际解析 "
                    f"{target_version}（请更新或删除 {EXCEPTIONS_FILE} 中该条）"
                )
                used_exceptions.add((workspace, via, via_version))
        distinct = {version for _, _, version in edges}
        if len(edges) == 1:
            workspace, via_source, target_version = edges[0]
            if explained:
                errors.append(
                    f"例外失效：{workspace} 是 {via}@{via_version} 分组的唯一"
                    f"边，无差异可豁免（请删除 {EXCEPTIONS_FILE} 中该条）"
                )
            elif len(errors) == errors_before:
                print(
                    f"{via} {via_version} -> {target_name} {target_version}"
                    f"（仅 {member_label(workspace, via_source)}，无比较对象）"
                )
        elif len(distinct) == 1:
            # 全组实际解析收敛（含全组例外收敛）：登记的例外一律冗余。
            for label, _, _ in explained:
                errors.append(
                    f"例外冗余：{label} 的 {via}@{via_version} -> "
                    f"{target_name} {next(iter(distinct))} 与全组一致，"
                    f"请删除 {EXCEPTIONS_FILE} 中该条"
                )
        elif len(baseline) > 1:
            lines = "\n".join(
                f"  {w} -> {v}" for v, ws in sorted(baseline.items()) for w in ws
            )
            errors.append(
                f"{via} {via_version} 的 {target_name} 解析不一致：\n{lines}\n"
                f"同一 {via} 版本线的 guest 依赖边须一致；确需差异请在 "
                f"{EXCEPTIONS_FILE} 登记例外（必填 reason）"
            )
        else:
            # 确有差异：非例外边收敛于单值，例外边不得与之同值。
            base = next(iter(baseline)) if baseline else None
            for label, version, _ in explained:
                if version == base:
                    errors.append(
                        f"例外冗余：{label} 的 {via}@{via_version} -> "
                        f"{target_name} {version} 与非例外边一致，"
                        f"请删除 {EXCEPTIONS_FILE} 中该条"
                    )
        if len(edges) > 1 and len(errors) == errors_before:
            members = ", ".join(
                member_label(ws, src)
                for ws, src, _ in sorted(edges, key=lambda e: (e[0], e[1] or "", e[2]))
            )
            if baseline:
                suffix = f"；{len(explained)} 条例内例外" if explained else ""
                print(
                    f"{via} {via_version} -> {target_name} "
                    f"{next(iter(baseline))}"
                    f"（{len(edges)} 条边：{members}）{suffix}"
                )
            else:
                detail = "、".join(f"{w}:{v}" for w, v, _ in explained)
                print(f"{via} {via_version} -> 全组例外（{detail}）")

    stale = set(exceptions) - used_exceptions
    for workspace, via, via_version in sorted(stale):
        errors.append(
            f"例外失效：{EXCEPTIONS_FILE} 声明 {workspace} 的 "
            f"{via}@{via_version} 例外，但该边当前不存在（请删除该条）"
        )

    for msg in errors:
        print(f"error: {msg}", file=sys.stderr)
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - 脚本化失败打印一行即可
        print(f"error: {e}", file=sys.stderr)
        raise SystemExit(1)
