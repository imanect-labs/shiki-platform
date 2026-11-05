#!/usr/bin/env python3
"""テナント表を読み書きする SQL に tenant 条件があるかを字句で検査する CI ゲート（#1034）。

不変条件「tenant_id を落とさない」（AGENTS.md）を、crates/data の S3（shard_safety_lint.rs）と同じ考え方で
全クレートへ広げる。toolchain 不要（python3 のみ）なので quality ジョブで毎回回す。

  T1: テナント表（migrations で tenant_id / tenant_int 列を持つ表）を FROM / JOIN / UPDATE / INTO で
      参照する SQL の文字列リテラルは、`tenant_id` か `tenant_int` を含む。

data_* 表は crates/data/src の中では S3 が同じ規則で見るので、そこでだけ対象外にする（二重に注記させない）。
crates/data の外から data_* 表を触る SQL はこのゲートが見る。

正当な例外（全テナントを横断する保守ジョブ・主キーが大域一意で tenant を引く前の照会など）は、リテラルの
開始行かその直前 3 行に `tenant-sql: allow(<理由>)` のコメントを書いて外す。理由は必須。

字句ヒューリスティックなので、format! で表名を組む SQL や、条件を別のリテラルに分けて push する
QueryBuilder は誤検知・見逃しがありうる。誤検知は注記で外し、見逃しは RLS（#1030）が多層で受ける。
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ALLOW = re.compile(r"tenant-sql:\s*allow\((?P<reason>[^)]*)\)")
SQL_VERB = re.compile(r"\b(select|insert|update|delete)\b", re.I)
TABLE_REF = re.compile(r"\b(?:from|join|update|into)\s+(?:only\s+)?([a-z_][a-z0-9_]*)\b", re.I)
TENANT_COND = re.compile(r"\btenant_(?:id|int)\b", re.I)
CREATE_TABLE = re.compile(
    r"create\s+table\s+(?:if\s+not\s+exists\s+)?([a-z_][a-z0-9_]*)\s*\((.*?)\)\s*(?:partition\s+by[^;]*)?;",
    re.I | re.S,
)
ADD_TENANT = re.compile(
    r"alter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?([a-z_][a-z0-9_]*)\s+add\s+column\s+(?:if\s+not\s+exists\s+)?tenant_(?:id|int)\b",
    re.I,
)


def tenant_tables(migrations: list[str]) -> set[str]:
    """migrations のうち tenant_id / tenant_int 列を持つ表。"""
    out: set[str] = set()
    for sql in migrations:
        for m in CREATE_TABLE.finditer(sql):
            if TENANT_COND.search(m.group(2)):
                out.add(m.group(1).lower())
        for m in ADD_TENANT.finditer(sql):
            out.add(m.group(1).lower())
    return out


def string_literals(src: str) -> list[tuple[int, str]]:
    """Rust ソースの文字列リテラル（開始行・中身）。コメント・char リテラル・ライフタイムを読み飛ばす。"""
    out: list[tuple[int, str]] = []
    i, line, n = 0, 1, len(src)

    def prev_ident(k: int) -> bool:
        return k > 0 and (src[k - 1].isalnum() or src[k - 1] == "_")

    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
        elif src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
        elif src.startswith("/*", i):
            end = src.find("*/", i + 2)
            end = n if end < 0 else end + 2
            line += src.count("\n", i, end)
            i = end
        elif c == "'":
            if src.startswith("\\", i + 1):
                end = src.find("'", i + 2)
                i = n if end < 0 else end + 1
            elif i + 2 < n and src[i + 2] == "'":
                i += 3
            else:
                i += 1
        elif c in "rb" and not prev_ident(i) and re.match(r"(?:b?r)(#*)\"", src[i:]):
            m = re.match(r"(?:b?r)(#*)\"", src[i:])
            hashes = m.group(1)
            start = i + m.end()
            end = src.find('"' + hashes, start)
            end = n if end < 0 else end
            out.append((line, src[start:end]))
            line += src.count("\n", i, end)
            i = end + 1 + len(hashes)
        elif c == '"':
            j, text, start_line = i + 1, [], line
            while j < n and src[j] != '"':
                if src[j] == "\\" and j + 1 < n:
                    if src[j + 1] == "\n":
                        line += 1
                    text.append(src[j + 1])
                    j += 2
                    continue
                if src[j] == "\n":
                    line += 1
                text.append(src[j])
                j += 1
            out.append((start_line, "".join(text)))
            i = j + 1
        else:
            i += 1
    return out


TEST_MOD = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([a-z]+\))?\s+)?mod\s+\w+\s*\{")


def test_ranges(src: str) -> list[tuple[int, int]]:
    """`#[cfg(test)] mod x { … }` の範囲（開始行・終了行）。中括弧を数え、文字列・コメント・char は飛ばす。

    ファイルの途中にあるテストモジュールの後ろの本番コードは、検査の対象に戻す。
    """
    ranges: list[tuple[int, int]] = []
    i, line, n, depth = 0, 1, len(src), 0
    open_at: list[tuple[int, int]] = []  # (テストモジュールが開いた深さ, 開始行)
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
        elif src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
        elif src.startswith("/*", i):
            end = src.find("*/", i + 2)
            end = n if end < 0 else end + 2
            line += src.count("\n", i, end)
            i = end
        elif c == "#" and (m := TEST_MOD.match(src, i)):
            line_start = line
            line += src.count("\n", i, m.end())
            depth += 1
            open_at.append((depth, line_start))
            i = m.end()
        elif c == "'" or c == '"' or (c in "rb" and re.match(r"b?r#*\"", src[i:])):
            # 文字列と char は string_literals と同じ規則で読み飛ばす（中の中括弧を数えない）。
            j = _skip_literal(src, i)
            line += src.count("\n", i, j)
            i = j
        elif c == "{":
            depth += 1
            i += 1
        elif c == "}":
            if open_at and open_at[-1][0] == depth:
                ranges.append((open_at.pop()[1], line))
            depth -= 1
            i += 1
        else:
            i += 1
    return ranges


def _skip_literal(src: str, i: int) -> int:
    """i から始まる文字列・char リテラル（またはライフタイム）の直後の位置。"""
    n = len(src)
    c = src[i]
    if c == "'":
        if src.startswith("\\", i + 1):
            end = src.find("'", i + 2)
            return n if end < 0 else end + 1
        if i + 2 < n and src[i + 2] == "'":
            return i + 3
        return i + 1
    m = re.match(r"b?r(#*)\"", src[i:])
    if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
        end = src.find('"' + m.group(1), i + m.end())
        return n if end < 0 else end + 1 + len(m.group(1))
    if c == '"':
        j = i + 1
        while j < n and src[j] != '"':
            j += 2 if src[j] == "\\" else 1
        return j + 1
    return i + 1


def violations(path: str, src: str, tables: set[str]) -> list[str]:
    lines = src.split("\n")
    tests = test_ranges(src)
    # crates/data/src の data_* は S3（shard_safety_lint.rs）が見る。
    if path.startswith("crates/data/src/"):
        tables = {t for t in tables if not t.startswith("data_")}
    out = []
    for line, text in string_literals(src):
        if any(a <= line <= b for a, b in tests):
            continue
        if not SQL_VERB.search(text):
            continue
        refs = sorted({t.lower() for t in TABLE_REF.findall(text)} & tables)
        if not refs or TENANT_COND.search(text):
            continue
        window = lines[max(0, line - 4) : line]
        allowed = [m for w in window if (m := ALLOW.search(w))]
        if allowed and allowed[-1].group("reason").strip():
            continue
        out.append(f"{path}:{line}: {', '.join(refs)}")
    return out


def is_test_path(path: str) -> bool:
    name = path.rsplit("/", 1)[-1]
    return "/tests/" in path or name == "tests.rs" or name.endswith("_tests.rs") or name.startswith("tests_")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", default=".")
    args = parser.parse_args(argv)
    root = Path(args.root)
    migrations = [p.read_text(encoding="utf-8") for p in sorted((root / "migrations").glob("*.sql"))]
    tables = tenant_tables(migrations)
    files = subprocess.run(
        ["git", "ls-files", "--", "crates/*/src/*.rs"], cwd=root, check=True, capture_output=True, text=True
    ).stdout.split()
    found = []
    for f in files:
        if is_test_path(f):
            continue
        found += violations(f, (root / f).read_text(encoding="utf-8"), tables)
    if found:
        print("❌ T1: テナント表を参照する SQL に tenant 条件（tenant_id / tenant_int）がありません:", file=sys.stderr)
        for v in found:
            print(v, file=sys.stderr)
        print(
            "\n条件を足すか、全テナントを横断する正当な処理なら直前 3 行以内に "
            "`// tenant-sql: allow(<理由>)` を書いてください。",
            file=sys.stderr,
        )
        return 1
    print(f"✅ tenant-sql gate: テナント表 {len(tables)} 個を参照する SQL はすべて tenant 条件か注記つき")
    return 0


if __name__ == "__main__":
    sys.exit(main())
