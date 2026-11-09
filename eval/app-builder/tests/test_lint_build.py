"""ir_lint のバイナリが作業ツリーより古ければ止めることと、作った commit を採点器の記録に残すこと（BUG-254）。

一時の git リポジトリに作業ツリーの形（crates/workflow-engine・依存のクレート・target/ の生成物）を作り、偽の
バイナリと cargo の依存の記録（`<バイナリ>.d`）を置いて、ファイルの時刻で確かめる（本物のビルドはしない）。
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

from abeval import cli, report
from abeval.grade import GRADER_VERSION, lint_build, static
from abeval.grade.static import LintError, grader_identity

pytestmark = [
    pytest.mark.shell,
    pytest.mark.skipif(shutil.which("git") is None, reason="git が要る"),
]

SOURCES = (
    "crates/workflow-engine/src/lib.rs",
    "crates/workflow-engine/examples/ir_lint.rs",
    "crates/data/src/query/field_use.rs",
)


def _git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        [
            "git",
            "-c",
            "user.name=abeval",
            "-c",
            "user.email=abeval@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            *args,
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    )
    return done.stdout.strip()


def _set_time(path: Path, t: float) -> None:
    os.utime(path, (t, t))


@pytest.fixture
def tree(tmp_path):
    """(作業ツリー, バイナリ)。ソースは時刻 1,000,000、バイナリはそれより新しい 2,000,000。"""
    repo = tmp_path / "repo"
    for rel in (*SOURCES, "crates/workflow-engine/src/x_tests.rs", "docs/a.md"):
        p = repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(f"// {rel}\n", encoding="utf-8")
    gen = repo / "target/debug/build/x/out/gen.rs"
    gen.parent.mkdir(parents=True)
    gen.write_text("// 生成物\n", encoding="utf-8")
    (repo / ".gitignore").write_text("target/\n", encoding="utf-8")
    _git(repo, "init", "-q")
    _git(repo, "add", ".")
    _git(repo, "commit", "-q", "-m", "first")
    exe = repo / "target/debug/examples/ir_lint"
    exe.parent.mkdir(parents=True)
    exe.write_text("#!/bin/sh\n", encoding="utf-8")
    exe.chmod(0o755)
    outside = tmp_path / "registry" / "serde" / "lib.rs"
    outside.parent.mkdir(parents=True)
    outside.write_text("//\n", encoding="utf-8")
    deps = [repo / s for s in SOURCES] + [gen, outside]
    # 空白を含むパスは `\ ` で書かれる（cargo の dep-info の形）。
    spaced = repo / "crates/data/src/with space.rs"
    spaced.write_text("//\n", encoding="utf-8")
    deps.append(spaced)
    line = f"{exe}: " + " ".join(str(d).replace(" ", "\\ ") for d in deps)
    (exe.parent / "ir_lint.d").write_text(line + "\n\n", encoding="utf-8")
    for p in [*repo.rglob("*"), outside]:
        if p.is_file():
            _set_time(p, 1_000_000)
    _set_time(exe, 2_000_000)
    return repo, exe


def test_a_fresh_binary_records_the_commit_and_a_clean_tree(tree):
    repo, exe = tree
    assert lint_build.stale_sources(exe, repo) == []
    info = lint_build.build_info(exe, repo)
    assert info["commit"] == _git(repo, "rev-parse", "HEAD")
    # 空白を含む untracked のソースは、git に載っていないので汚れとして数える。
    assert info["dirty"] is True
    _git(repo, "add", ".")
    _git(repo, "commit", "-q", "-m", "second")
    _set_time(exe, 3_000_000)
    info = lint_build.build_info(exe, repo)
    assert info == {
        "commit": _git(repo, "rev-parse", "HEAD"),
        "dirty": False,
        "built_at": "1970-02-04T17:20:00+00:00",
    }


def test_a_source_newer_than_the_binary_stops_with_the_rebuild_command(tree):
    repo, exe = tree
    _set_time(repo / "crates/data/src/query/field_use.rs", 2_500_000)
    assert lint_build.stale_sources(exe, repo) == ["crates/data/src/query/field_use.rs"]
    with pytest.raises(lint_build.StaleLint) as e:
        lint_build.check_fresh(exe, repo)
    assert "crates/data/src/query/field_use.rs" in str(e.value)
    assert lint_build.REBUILD in str(e.value)
    with pytest.raises(lint_build.StaleLint):
        lint_build.build_info(exe, repo)


def test_files_cargo_does_not_read_do_not_stop_grading(tree):
    """テストだけのファイル・docs・target/ の生成物・作業ツリーの外は、新しくても止めない（作り直しても時刻が
    変わらず、止まり続けることがない）。"""
    repo, exe = tree
    for rel in (
        "crates/workflow-engine/src/x_tests.rs",
        "docs/a.md",
        "target/debug/build/x/out/gen.rs",
    ):
        _set_time(repo / rel, 2_500_000)
    _set_time(repo.parent / "registry/serde/lib.rs", 2_500_000)
    assert lint_build.stale_sources(exe, repo) == []


def test_a_removed_source_or_a_missing_record_stops(tree):
    repo, exe = tree
    (repo / "crates/workflow-engine/src/lib.rs").unlink()
    assert lint_build.stale_sources(exe, repo) == ["crates/workflow-engine/src/lib.rs（消えた）"]
    (exe.parent / "ir_lint.d").write_text(f"{exe}: {repo / 'crates/data/src/query/field_use.rs'}\n")
    with pytest.raises(lint_build.StaleLint, match="crates/workflow-engine のファイルがありません"):
        lint_build.check_fresh(exe, repo)
    (exe.parent / "ir_lint.d").unlink()
    with pytest.raises(lint_build.StaleLint, match="依存の記録（ir_lint.d）が無い"):
        lint_build.check_fresh(exe, repo)


def test_the_grader_version_carries_the_build_and_refuses_a_stale_binary(tree, monkeypatch, capsys):
    repo, exe = tree
    monkeypatch.setattr(lint_build, "REPO", repo)
    g = static.grader_version(exe)
    assert g["ir_lint_build"]["commit"] == _git(repo, "rev-parse", "HEAD")
    assert set(grader_identity(g)) == {"grader", "ir_lint_schema", "ir_lint_sha256"}
    _set_time(repo / "crates/workflow-engine/src/lib.rs", 2_500_000)
    with pytest.raises(LintError, match="作業ツリーより古い"):
        static.grader_version(exe)
    with pytest.raises(LintError, match="作業ツリーより古い"):
        static.run_ir_lint([("a", {})], exe)
    # 採点の前（`abeval grade`）に止めて、作り直しの命令を出す（1 で終わる）。
    doc = {"experiment": "stale", "trials": [], "grader": {"grader": GRADER_VERSION}}
    report.write_result(doc, [], merge=False)
    assert cli.main(["--exp", "stale", "--lint-bin", str(exe)], command="grade") == 1
    assert "作業ツリーより古い" in capsys.readouterr().err


def test_the_build_is_not_part_of_the_grader_identity():
    """作った commit だけが違う採点器の記録は、同じ採点器として重ね・並べる（試行を捨てない）。"""
    base = {"grader": 11, "ir_lint_schema": 3, "ir_lint_sha256": "aa"}
    a = {**base, "ir_lint_build": {"commit": "c1", "dirty": False, "built_at": "x"}}
    b = {**base, "ir_lint_build": {"commit": "c2", "dirty": True, "built_at": "y"}}
    trial = {"label": "vf", "mode": "a", "task": "t", "phrasing": "flow", "n": 0}
    merged = report.merge_result(
        {"grader": a, "trials": [{**trial, "n": 1}]}, {"grader": b, "trials": [trial]}
    )
    assert len(merged["trials"]) == 2 and merged["merge_notes"] == []
    keep, left = report.same_grader([{"grader": a}, {"grader": b}])
    assert len(keep) == 2 and left == []
    other = report.merge_result(
        {"grader": a, "trials": [{**trial, "n": 1}]},
        {"grader": {**b, "ir_lint_sha256": "bb"}, "trials": [trial]},
    )
    assert len(other["trials"]) == 1 and other["merge_notes"][0]["dropped_trials"] == 1
