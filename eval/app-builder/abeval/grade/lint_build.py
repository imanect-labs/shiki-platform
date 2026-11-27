"""ir_lint のバイナリが作業ツリーより古くないかと、作った commit（BUG-254）。

cargo は example を作るたびに、使ったソースの一覧（dep-info・`<バイナリ>.d`）をバイナリの隣に書く。採点の前に、
その一覧のうち作業ツリーの中のファイル（`crates/workflow-engine` と、検証が使う data・script-runtime などの依存の
クレート。`target/` の中の生成物は除く）が、バイナリより新しくないかを確かめる。新しいもの・消えたものがあれば、
古い規則で S2s を数えないよう止めて、作り直しの命令を出す（出力の形の版〔schema〕が同じでも止める）。

一覧は cargo が実際に読んだファイルなので、cargo が作り直さないファイル（テストだけの `*_tests.rs`・docs など）の
変更では止めない（作り直しても時刻が変わらず、止まり続けることがない）。

cargo は一覧に Cargo.toml・Cargo.lock を載せず、レジストリのクレートも外す。依存の版の上げ下げ（Cargo.lock）・feature の
切り替え（クレートの Cargo.toml）・ツールチェーンの指定の変更は .rs の時刻を変えないので、一覧のソースのクレートの
Cargo.toml と、作業ツリーの根の Cargo.toml・Cargo.lock・ツールチェーンの指定（[`BUILD_FILES`]）も比べる。これらは
変わっても cargo が作り直さないことがある（ir_lint の依存に関わらない版の変更など）ので、新しいときは example の
ソースの時刻を進めてから作り直す命令（[`REBUILD_FORCED`]）を出す（必ず作り直されて時刻が進み、止まり続けない）。

採点器の記録（`grader_version()` の `ir_lint_build`）には、確かめた時点の HEAD の commit と、一覧のファイルと
比べたビルドのファイルに commit していない変更があったか（`dirty`）を残す。バイナリがそのどれよりも新しいので、その
バイナリは「その commit のそれらのファイル（`dirty` なら作業ツリーの中身）」から作ったものとみなせる。結果を比べるときの採点器の同一性は
`static.grader_identity`（GRADER_VERSION・出力の形の版・バイナリの sha）で見て、作った commit は比べない（中身の
同じバイナリを、関係の無い commit のたびに別の採点器と数えない）。

限界: ビルドの途中でファイルを書き換えると、書き換える前の中身で作ったバイナリでも新しい時刻になる（気づけない）。
"""

from __future__ import annotations

import datetime as dt
import re
import subprocess
from pathlib import Path

from abeval.config import ROOT

REPO = ROOT.parent.parent
REBUILD = "cargo build -p shiki-workflow-engine --example ir_lint"
ENGINE = Path("crates") / "workflow-engine"
# ビルドのファイルだけが新しいときの作り直し（cargo が作り直さないと時刻が進まないので、example のソースの時刻を進める）。
REBUILD_FORCED = f"touch {ENGINE / 'examples' / 'ir_lint.rs'} && {REBUILD}"
# dep-info に載らないが、ビルドの中身を変える作業ツリーの根のファイル（無いものは数えない）。
BUILD_FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo/config.toml",
)
_UNESCAPED_SPACE = re.compile(r"(?<!\\) ")


class StaleLint(RuntimeError):
    """ir_lint が作業ツリーより古い・古くないかを確かめられない（`static.LintError` として投げ直す）。"""


def dep_info_path(bin_path: Path) -> Path:
    return bin_path.with_name(bin_path.name + ".d")


def read_dep_info(path: Path) -> list[Path]:
    """dep-info（`<出力>: <ソース> <ソース> …`）のソースの一覧。空白は `\\ ` で書かれる。"""
    text = path.read_text(encoding="utf-8")
    first = next((line for line in text.splitlines() if line.strip()), "")
    _, sep, deps = first.partition(": ")
    if not sep:
        return []
    return [Path(p.replace("\\ ", " ")) for p in _UNESCAPED_SPACE.split(deps.strip()) if p]


def repo_sources(deps: list[Path], repo: Path | None = None) -> list[Path]:
    """作業ツリーの中のソース（作業ツリーからの相対・`target/` の中は除く）。"""
    out = []
    root = (repo or REPO).resolve()
    for p in deps:
        try:
            rel = p.resolve().relative_to(root)
        except ValueError:
            continue
        if rel.parts and rel.parts[0] != "target":
            out.append(rel)
    return out


def build_files(sources: list[Path], repo: Path | None = None) -> list[Path]:
    """dep-info に載らないが、バイナリの中身を変えるファイル（作業ツリーからの相対・あるものだけ）。

    作業ツリーの根の [`BUILD_FILES`] と、一覧のソースのクレートの Cargo.toml（ソースから上へ辿って最初の Cargo.toml）。
    """
    root = repo or REPO
    out = {Path(f) for f in BUILD_FILES if (root / f).is_file()}
    for s in sources:
        for parent in s.parents:
            if parent == Path("."):
                break
            if (root / parent / "Cargo.toml").is_file():
                out.add(parent / "Cargo.toml")
                break
    return sorted(out)


def _dep_sources(bin_path: Path, repo: Path) -> list[Path]:
    """一覧の作業ツリーの中のソース（記録が無い・workflow-engine のファイルが無ければ StaleLint）。"""
    d = dep_info_path(bin_path)
    if not d.is_file():
        raise StaleLint(
            f"ir_lint の依存の記録（{d.name}）が無いので、作業ツリーより古くないかを確かめられません。"
            f"{REBUILD} で作り直します"
        )
    sources = repo_sources(read_dep_info(d), repo)
    if not any(s.parts[: len(ENGINE.parts)] == ENGINE.parts for s in sources):
        raise StaleLint(
            f"ir_lint の依存の記録（{d.name}）に {ENGINE} のファイルがありません（この作業ツリーで作った"
            f"バイナリではない）。{REBUILD} で作り直します"
        )
    return sources


def stale_sources(bin_path: Path, repo: Path | None = None) -> list[str]:
    """バイナリより新しい（か、消えた）作業ツリーのソースとビルドのファイル。確かめられなければ StaleLint。"""
    repo = repo or REPO
    sources = _dep_sources(bin_path, repo)
    built = bin_path.stat().st_mtime_ns
    out = []
    for rel in [*sources, *build_files(sources, repo)]:
        try:
            if (repo / rel).stat().st_mtime_ns > built:
                out.append(str(rel))
        except FileNotFoundError:
            out.append(f"{rel}（消えた）")
    return sorted(out)


def check_fresh(bin_path: Path, repo: Path | None = None) -> None:
    """ir_lint が作業ツリーのソースより古ければ StaleLint（作り直しの命令を添える）。"""
    stale = stale_sources(bin_path, repo)
    if stale:
        more = f" ほか {len(stale) - 3} 件" if len(stale) > 3 else ""
        # ソースが新しければ cargo は必ず作り直す。ビルドのファイルだけなら作り直さないことがあるので、時刻を進める。
        manifests_only = all(Path(s).name in {Path(f).name for f in BUILD_FILES} for s in stale)
        rebuild = REBUILD_FORCED if manifests_only else REBUILD
        raise StaleLint(
            f"ir_lint（{bin_path}）が作業ツリーより古いです（新しいファイル: {'・'.join(stale[:3])}{more}）。"
            f"古い検証の規則で採点しないよう止めます。{rebuild} で作り直してから流し直します"
        )


def _git(repo: Path, *args: str) -> str | None:
    try:
        done = subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout if done.returncode == 0 else None


def build_info(bin_path: Path, repo: Path | None = None) -> dict:
    """採点器の記録に残す、ir_lint を作った commit（古ければ StaleLint）。

    `commit` は確かめた時点の HEAD、`dirty` は一覧のファイルとビルドのファイル（[`build_files`]）に commit していない
    変更があったか（git で読めなければどちらも None）、`built_at` はバイナリの時刻（UTC）。
    """
    repo = repo or REPO
    check_fresh(bin_path, repo)
    sources = _dep_sources(bin_path, repo)
    files = [str(s) for s in [*sources, *build_files(sources, repo)]]
    head = _git(repo, "rev-parse", "HEAD")
    status = _git(repo, "status", "--porcelain", "--", *files) if head is not None else None
    built = dt.datetime.fromtimestamp(bin_path.stat().st_mtime, dt.UTC)
    return {
        "commit": head.strip() if head else None,
        "dirty": None if status is None else bool(status.strip()),
        "built_at": built.isoformat(timespec="seconds"),
    }
