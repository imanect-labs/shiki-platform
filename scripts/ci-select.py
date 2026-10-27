#!/usr/bin/env python3
"""CI で回すジョブ・テストを、PR の変更ファイルから選ぶ（#921）。

関係ない変更で全テストを回さないための選択器。安全側に倒すのが原則で、判定できないものは全量にする。

  select         変更ファイル → 回すジョブ（rust / web / python / compose）と、Rust テストの対象パッケージ
                 （変更したクレート＋それに依存する全クレート）と、常に回す脅威テストを出す。
  diff-coverage  lcov と PR の差分から、変更した行のカバレッジを出して閾値で判定する
                 （PR は変更行で判定し、全体 80% のゲートは main で判定する・#921 の human 決定）。

依存は標準ライブラリのみ（changes ジョブに Rust ツールチェーンを入れない）。Cargo.toml は tomllib で読む。
ロジックの検査は scripts/test_ci_select.py（quality ジョブで回す）。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

# 変更されたら全量にするパス。依存グラフ・ツールチェーン・CI 自体・全テストが共有する入力
# （migrations は Postgres を使う全テストが流す）・compose（結合テストの実行環境）。
# Cargo.lock は含めない: 差分から実際に変わったパッケージを求め、それに依存するクレートだけを選ぶ
# （lock_changed_packages）。読めなければ全量。
FULL_TRIGGERS = (
    "Cargo.toml",
    "rust-toolchain.toml",
    "deny.toml",
    ".config/",
    ".cargo/",
    ".github/workflows/ci.yml",
    "scripts/ci-select.py",
    "vendor/",
    "migrations/",
    "deploy/compose/",
)

# CI の起動条件（ci.yml の paths-ignore）と同じ。選択の判断に使わない。最上位の *.md だけを外し、
# crates/ の下の .md はそのクレートの変更として扱う（include_str! でコンパイルに入るものがある・
# 例 crates/chat/src/worker/yui_guide.md）。
IGNORED_PREFIXES = ("docs/", ".claude/")

# パッケージ名は nextest の式・cargo の引数・GITHUB_OUTPUT に流すので、想定外の文字は通さない。
PACKAGE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")

# 選択に関係なく毎回回す脅威テスト（パッケージ, テストバイナリ）。認可バイパス・テナント越境を
# 守る結合テストで、変更したクレートから依存を辿って届かない変更（設定・実行時の入力）でも落としたくない。
ALWAYS_RUN = (
    ("shiki-authz", "check"),
    ("shiki-authz", "retenant"),
    ("shiki-data", "policy_threat_it"),
    ("shiki-data", "authz_memo_it"),
    ("shiki-rag", "search_authz_it"),
    ("shiki-storage", "tenant_it"),
    ("shiki-sandbox-orchestrator", "tenant_isolation"),
    ("shiki-llm-gateway", "tenant_usage_it"),
)

DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def workspace_packages(root: Path) -> dict[str, dict]:
    """crates/* の各パッケージ名 → {dir, deps}。deps は workspace 内の依存先パッケージ名（dev / build を含む）。"""
    root_manifest = tomllib.loads((root / "Cargo.toml").read_text())
    ws_deps = root_manifest.get("workspace", {}).get("dependencies", {})
    members: dict[str, dict] = {}
    raw: dict[str, dict] = {}
    for manifest in sorted(root.glob("crates/*/Cargo.toml")):
        data = tomllib.loads(manifest.read_text())
        name = data.get("package", {}).get("name")
        if not name:
            continue
        if not PACKAGE_NAME.match(name):
            raise ValueError(f"想定外のパッケージ名: {name!r}（{manifest}）")
        raw[name] = data
        members[name] = {"dir": manifest.parent.relative_to(root).as_posix(), "deps": set()}

    def resolve(key: str, spec) -> str:
        if isinstance(spec, dict):
            if spec.get("workspace"):
                ws = ws_deps.get(key, {})
                return ws.get("package", key) if isinstance(ws, dict) else key
            return spec.get("package", key)
        return key

    for name, data in raw.items():
        tables = [data.get(t, {}) for t in DEP_TABLES]
        for target in data.get("target", {}).values():
            tables += [target.get(t, {}) for t in DEP_TABLES]
        for table in tables:
            for key, spec in table.items():
                dep = resolve(key, spec)
                if dep in members and dep != name:
                    members[name]["deps"].add(dep)
    return members


def reverse_closure(members: dict[str, dict], seeds: set[str]) -> set[str]:
    """seeds とそれに（推移的に）依存する全パッケージ。"""
    selected = set(seeds)
    changed = True
    while changed:
        changed = False
        for name, info in members.items():
            if name not in selected and info["deps"] & selected:
                selected.add(name)
                changed = True
    return selected


def owner(members: dict[str, dict], path: str) -> str | None:
    """path を含む最も深いパッケージ（crates/tabular/runner/… → shiki-tabular）。"""
    best = None
    for name, info in members.items():
        d = info["dir"] + "/"
        if path.startswith(d) and (best is None or len(d) > len(members[best]["dir"]) + 1):
            best = name
    return best


def is_ignored(path: str) -> bool:
    if path.startswith(IGNORED_PREFIXES):
        return True
    return "/" not in path and path.endswith(".md")


def parse_lock(text: str) -> dict[str, dict]:
    """Cargo.lock → {"名前 版": {"name", "id"（版・出所・checksum）, "deps"（依存先の名前）}}。"""
    data = tomllib.loads(text)
    out: dict[str, dict] = {}
    for pkg in data.get("package", []):
        key = f"{pkg['name']} {pkg['version']}"
        out[key] = {
            "name": pkg["name"],
            # 依存の一覧も比べる（workspace のクレートは版が 0.0.0 のまま依存だけが変わる）。
            "id": (pkg.get("version"), pkg.get("source"), pkg.get("checksum"),
                   tuple(sorted(pkg.get("dependencies", [])))),
            # 依存は "name" / "name version" / "name version (source)"。名前で辿る（同名の版違いは
            # まとめて扱うので選び過ぎるだけで、漏れはしない）。
            "deps": {d.split(" ")[0] for d in pkg.get("dependencies", [])},
        }
    return out


def lock_changed_packages(old: str, new: str, members: dict[str, dict]) -> set[str]:
    """Cargo.lock の差分で変わった依存を（推移的に）使う workspace クレート。"""
    before, after = parse_lock(old), parse_lock(new)
    changed = {after[k]["name"] for k in after if k not in before or before[k]["id"] != after[k]["id"]}
    changed |= {before[k]["name"] for k in before if k not in after}
    users = set(changed)
    grew = True
    while grew:
        grew = False
        for pkg in after.values():
            if pkg["name"] not in users and pkg["deps"] & users:
                users.add(pkg["name"])
                grew = True
    return users & set(members)


def select(members: dict[str, dict], files: list[str], full_run: bool = False,
           lock: tuple[str, str] | None = None) -> dict:
    """lock は Cargo.lock の（base, head）の中身。Cargo.lock が変わったときだけ渡す。"""
    files = [f for f in files if f and not is_ignored(f)]
    reasons: list[str] = []
    full = full_run
    if full_run:
        reasons.append("全量（main への push）")
    seeds: set[str] = set()
    web = python = False
    for f in files:
        if any(f == t or (t.endswith("/") and f.startswith(t)) for t in FULL_TRIGGERS):
            full = True
            reasons.append(f"全量: {f}")
        elif f == "Cargo.lock":
            try:
                if lock is None:
                    raise ValueError("Cargo.lock の中身が渡されていない")
                used = lock_changed_packages(lock[0], lock[1], members)
            except (ValueError, KeyError, tomllib.TOMLDecodeError) as e:
                full = True
                reasons.append(f"全量（Cargo.lock を読めない: {e}）")
            else:
                seeds |= used
                reasons.append(f"Cargo.lock: 変わった依存を使うクレート {len(used)} 件")
        elif re.fullmatch(r"crates/[^/]+/Cargo\.toml", f):
            # 依存・フィーチャの変更は、フィーチャ統合で依存を共有する他のクレートのビルドも変える。
            full = True
            reasons.append(f"全量（クレートの依存定義）: {f}")
        elif f.startswith("crates/"):
            pkg = owner(members, f)
            if pkg is None:
                full = True
                reasons.append(f"全量（どのクレートにも属さない）: {f}")
            else:
                seeds.add(pkg)
        elif f.startswith("web/"):
            web = True
        elif f.startswith("ingestion-worker/"):
            python = True
        else:
            full = True
            reasons.append(f"全量（選択規則に無いパス）: {f}")

    rust = full or bool(seeds)
    packages = sorted(members) if full else sorted(reverse_closure(members, seeds))
    out = {
        "full": full,
        "rust": rust,
        # web の型は Rust 定義から生成する（gen:api）ので、Rust の変更でも web を回す。
        "web": full or web or rust,
        "python": full or python,
        # compose（smoke / E2E）は shiki-server と web の両方を載せる。
        "compose": full or web or rust,
        "packages": packages,
        "reasons": reasons,
    }
    out["nextest_filter"] = nextest_filter(out)
    out["cargo_packages"] = " ".join(f"-p {p}" for p in packages)
    # 脅威テストは 1 回の `cargo llvm-cov --workspace --test …` で回す（パッケージごとに起動すると
    # 依存のフィーチャ集合が変わって計装ビルドがやり直しになる）。テスト名は workspace 内で一意。
    out["always_run_tests"] = " ".join(f"--test {t}" for _, t in ALWAYS_RUN)
    return out


def nextest_filter(sel: dict) -> str:
    if sel["full"]:
        return "all()"
    # `=` は完全一致（式を組み立てるときは matcher の種類を明示する・nextest の filterset reference）。
    terms = [f"package(={p})" for p in sel["packages"]]
    terms += [f"binary_id(={p}::{t})" for p, t in ALWAYS_RUN]
    return " | ".join(terms)


# 利用者の git 設定（diff.noprefix・quotePath・外部 diff）に左右されない出力にする。
GIT_DIFF = ["git", "-c", "core.quotePath=false", "diff", "--no-ext-diff", "--no-textconv"]


def changed_files(base: str, head: str) -> list[str]:
    out = subprocess.run(
        [*GIT_DIFF, "--name-only", "--no-renames", base, head],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [line for line in out.splitlines() if line]


def git_show(rev: str, path: str) -> str:
    return subprocess.run(["git", "show", f"{rev}:{path}"], check=True, capture_output=True, text=True).stdout


def changed_lines(base: str, head: str) -> dict[str, set[int]]:
    """*.rs の追加・変更行（新しい側の行番号）。移動・改名は検出し、動かしただけの行は数えない。"""
    out = subprocess.run(
        [*GIT_DIFF, "--unified=0", "-M", "--src-prefix=a/", "--dst-prefix=b/", base, head, "--", "*.rs"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return parse_changed_lines(out)


HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def parse_changed_lines(diff: str) -> dict[str, set[int]]:
    result: dict[str, set[int]] = {}
    current: str | None = None
    for line in diff.splitlines():
        if line.startswith("+++ "):
            # 空白を含むパスでは git が末尾にタブを付ける。
            target = line[4:].rstrip("\t")
            current = target[2:] if target.startswith("b/") else None
            continue
        m = HUNK.match(line)
        if m and current:
            start, count = int(m.group(1)), int(m.group(2) or "1")
            result.setdefault(current, set()).update(range(start, start + count))
    return result


def parse_lcov(text: str, root: str) -> dict[str, dict[int, int]]:
    """lcov → {リポジトリ相対パス: {行: 実行回数}}。"""
    result: dict[str, dict[int, int]] = {}
    current: dict[int, int] | None = None
    prefix = root.rstrip("/") + "/"
    for line in text.splitlines():
        if line.startswith("SF:"):
            path = line[3:]
            if path.startswith(prefix):
                path = path[len(prefix):]
            current = result.setdefault(path, {})
        elif line.startswith("DA:") and current is not None:
            no, hits = line[3:].split(",")[:2]
            current[int(no)] = current.get(int(no), 0) + int(hits)
        elif line == "end_of_record":
            current = None
    return result


def diff_coverage(lines: dict[str, set[int]], lcov: dict[str, dict[int, int]], ignore: str | None) -> dict:
    """変更行のうち実行可能な行（lcov に DA がある行）の何割が実行されたか。"""
    pattern = re.compile(ignore) if ignore else None
    files = []
    total = covered = 0
    for path in sorted(lines):
        if pattern and pattern.search(path):
            continue
        da = lcov.get(path)
        if not da:
            continue
        executable = sorted(n for n in lines[path] if n in da)
        if not executable:
            continue
        missed = [n for n in executable if da[n] == 0]
        total += len(executable)
        covered += len(executable) - len(missed)
        files.append({"path": path, "executable": len(executable), "missed": missed})
    percent = 100.0 if total == 0 else covered * 100.0 / total
    return {"total": total, "covered": covered, "percent": percent, "files": files}


def unmatched_sources(lines: dict[str, set[int]], lcov: dict[str, dict[int, int]], ignore: str | None,
                      members: dict[str, dict]) -> list[str]:
    """変更した src/*.rs のうち、属するクレートが lcov に 1 件も無いもの（無ければ空）。

    lcov と diff のパスが食い違う・選んだクレートが計測されていない、のどちらでも変更行が 1 行も数えられず
    「100% で合格」に倒れる。黙って通さないよう検出して落とす。ファイル単位では判定しない: 文字列定数だけの
    ファイルは実行可能な行が無く、正しく計測しても lcov に現れない（例 agent-core/src/tools/subagent_prompts.rs）。
    """
    pattern = re.compile(ignore) if ignore else None
    measured = {name for name, info in members.items()
                if any(path.startswith(info["dir"] + "/src/") for path in lcov)}
    missing = []
    for path in sorted(lines):
        if pattern and pattern.search(path):
            continue
        crate = next((name for name, info in members.items() if path.startswith(info["dir"] + "/src/")), None)
        if crate is not None and crate not in measured:
            missing.append(path)
    return missing


def md_code(text: str) -> str:
    """ファイル名を Markdown の表に安全に埋め込む（バッククォート・縦棒で表やコメントを崩さない）。"""
    return "`" + text.replace("`", "'").replace("|", "\\|") + "`"


def format_ranges(numbers: list[int]) -> str:
    ranges, start, prev = [], None, None
    for n in numbers:
        if start is None:
            start = prev = n
        elif n == prev + 1:
            prev = n
        else:
            ranges.append(f"{start}" if start == prev else f"{start}-{prev}")
            start = prev = n
    if start is not None:
        ranges.append(f"{start}" if start == prev else f"{start}-{prev}")
    return ", ".join(ranges)


def write_outputs(path: str | None, values: dict) -> None:
    lines = []
    for key, value in values.items():
        if isinstance(value, bool):
            value = "true" if value else "false"
        elif isinstance(value, list):
            value = " ".join(value)
        lines.append(f"{key}={value}")
    if path:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    print("\n".join(lines))


def cmd_select(args: argparse.Namespace) -> int:
    root = Path(args.root)
    members = workspace_packages(root)
    files = [] if args.full else changed_files(args.base, args.head)
    lock = None
    if "Cargo.lock" in files:
        try:
            lock = (git_show(args.base, "Cargo.lock"), git_show(args.head, "Cargo.lock"))
        except subprocess.CalledProcessError:
            lock = None  # 片側に無い（追加・削除）: select が全量に倒す
    sel = select(members, files, full_run=args.full, lock=lock)
    for reason in sel["reasons"]:
        print(f"# {reason}", file=sys.stderr)
    write_outputs(
        args.github_output,
        {k: sel[k] for k in ("full", "rust", "web", "python", "compose", "nextest_filter", "cargo_packages", "always_run_tests")},
    )
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as fh:
            fh.write("### テストの選択（scripts/ci-select.py）\n\n")
            fh.write(f"- 全量: {sel['full']}\n")
            for key in ("rust", "web", "python", "compose"):
                fh.write(f"- {key}: {sel[key]}\n")
            if not sel["full"]:
                fh.write(f"- Rust の対象パッケージ（{len(sel['packages'])}）: {', '.join(sel['packages']) or 'なし'}\n")
            for reason in sel["reasons"][:20]:
                fh.write(f"- {reason}\n")
    return 0


def cmd_diff_coverage(args: argparse.Namespace) -> int:
    lines = changed_lines(args.base, args.head)
    lcov = parse_lcov(Path(args.lcov).read_text(), args.root)
    missing = unmatched_sources(lines, lcov, args.ignore, workspace_packages(Path(args.root)))
    if missing:
        text = (
            "変更行カバレッジを計測できません: 変更したファイルのクレートが lcov に 1 件もありません"
            "（lcov と diff のパスの食い違い、またはそのクレートのテストが回っていない）。\n\n"
            + "\n".join(f"- {md_code(p)}" for p in missing[:20]) + "\n"
        )
        print(text)
        if args.output:
            Path(args.output).write_text(text, encoding="utf-8")
        return 2
    result = diff_coverage(lines, lcov, args.ignore)
    report = [
        f"変更行カバレッジ: **{result['percent']:.1f}%**（実行可能な変更行 {result['total']} 行中 {result['covered']} 行・閾値 {args.min:g}%）",
        "",
    ]
    uncovered = [f for f in result["files"] if f["missed"]]
    if uncovered:
        report.append("| ファイル | 実行されていない変更行 |")
        report.append("| --- | --- |")
        for f in uncovered:
            report.append(f"| {md_code(f['path'])} | {format_ranges(f['missed'])} |")
    text = "\n".join(report) + "\n"
    print(text)
    if args.output:
        Path(args.output).write_text(text, encoding="utf-8")
    if args.json:
        Path(args.json).write_text(json.dumps(result, ensure_ascii=False), encoding="utf-8")
    return 0 if result["percent"] >= args.min else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("select")
    p.add_argument("--base", default="HEAD^1")
    p.add_argument("--head", default="HEAD")
    p.add_argument("--full", action="store_true", help="変更に関係なく全量（main への push）")
    p.add_argument("--root", default=".")
    p.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT"))
    p.add_argument("--summary", default=os.environ.get("GITHUB_STEP_SUMMARY"))
    p.set_defaults(func=cmd_select)

    d = sub.add_parser("diff-coverage")
    d.add_argument("--lcov", required=True)
    d.add_argument("--base", default="HEAD^1")
    d.add_argument("--head", default="HEAD")
    d.add_argument("--root", default=os.getcwd())
    d.add_argument("--ignore", help="除外するファイルの正規表現（llvm-cov の --ignore-filename-regex と同じもの）")
    d.add_argument("--min", type=float, default=80.0)
    d.add_argument("--output", help="結果の Markdown を書き出す先")
    d.add_argument("--json")
    d.set_defaults(func=cmd_diff_coverage)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
