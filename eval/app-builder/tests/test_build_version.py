"""scripts/build-version.sh（版のビルドと退避・GAP-30 §4.1）。

小さな git リポジトリを一時の場所に作り、偽の cargo（`ABEVAL_CARGO`）で流す（本物のビルドはしない）。
"""

from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path

import pytest

import shellkit
from shellkit import BUILD_VERSION

pytestmark = [
    pytest.mark.shell,
    pytest.mark.skipif(not shellkit.bash_available(), reason="bash と git が要る"),
]

# 偽の cargo: 呼ばれた引数と場所を記録し、CARGO_TARGET_DIR にバイナリを置く（中身に commit を入れる）。
FAKE_CARGO = r"""#!/usr/bin/env bash
printf 'cargo %s cwd=%s target=%s\n' "$*" "$PWD" "${CARGO_TARGET_DIR:-}" >> "$SHIM_LOG"
[ -n "${FAKE_CARGO_FAIL:-}" ] && exit 101
[ -n "${FAKE_CARGO_TOUCH:-}" ] && echo changed >> README.md
# 古い lock の再現: ランナーを --locked でビルドすると断り、--offline なら lock に足す。
if [ -n "${FAKE_CARGO_STALE_LOCK:-}" ]; then
  case "$*" in
    *--locked*--manifest-path*) echo "error: cannot update the lock file because --locked was passed" >&2; exit 101 ;;
    *--offline*--manifest-path*) printf '\n[[package]]\nname = "added-dep"\nversion = "1.0.0"\n' >> crates/tabular/runner/Cargo.lock ;;
  esac
fi
mkdir -p "$CARGO_TARGET_DIR/debug"
case "$*" in
  *--manifest-path*) out="$CARGO_TARGET_DIR/debug/shiki-tabular-runner"; what=runner ;;
  *) out="$CARGO_TARGET_DIR/debug/shiki-server"; what=server ;;
esac
printf 'fake-%s %s\n' "$what" "$(git rev-parse HEAD)" > "$out"
chmod +x "$out"
"""

FAKE_RUSTC = """#!/usr/bin/env bash
echo "rustc 1.99.0 (abeval 2026-01-01)"
"""


class Box:
    def __init__(self, tmp: Path):
        self.tmp = tmp
        self.log = tmp / "shim.log"
        tools = tmp / "tools"
        cargo = shellkit.write_exe(tools / "cargo", FAKE_CARGO)
        rustc = shellkit.write_exe(tools / "rustc", FAKE_RUSTC)
        self.env = shellkit.base_env(
            tmp, None, self.log, {"ABEVAL_CARGO": str(cargo), "ABEVAL_RUSTC": str(rustc)}
        )
        self.repo = tmp / "repo"
        self.repo.mkdir()
        g = self.git
        g("init", "-q")
        (self.repo / "README.md").write_text("abeval test\n")
        (self.repo / ".gitignore").write_text("target/\n")
        runner = self.repo / "crates" / "tabular" / "runner"
        runner.mkdir(parents=True)
        (runner / "Cargo.toml").write_text('[package]\nname = "shiki-tabular-runner"\n')
        (runner / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "duckdb"\nversion = "1.0.0"\n'
        )
        g("add", ".")
        g("commit", "-q", "-m", "first")
        self.first = g("rev-parse", "HEAD")
        (self.repo / "README.md").write_text("abeval test 2\n")
        g("commit", "-q", "-am", "second")
        self.head = g("rev-parse", "HEAD")
        self.wt_root = tmp / "worktrees"
        self.bin_dir = tmp / "bin"
        self.versions = tmp / "versions"

    def git(self, *args: str) -> str:
        return shellkit.git(self.repo, *args, env=self.env)

    def run(self, *args: str, **env) -> shellkit.Run:
        return shellkit.run_bash(
            BUILD_VERSION,
            [
                "--repo",
                str(self.repo),
                "--worktree-root",
                str(self.wt_root),
                "--bin-dir",
                str(self.bin_dir),
                "--versions-dir",
                str(self.versions),
                *args,
            ],
            cwd=self.tmp,
            env={**self.env, **env},
            log=self.log,
        )

    def worktrees(self) -> list[str]:
        out = self.git("worktree", "list", "--porcelain")
        return [line.split(" ", 1)[1] for line in out.splitlines() if line.startswith("worktree ")]


@pytest.fixture
def box(tmp_path):
    return Box(tmp_path)


def test_builds_records_and_removes_the_worktree(box):
    r = box.run("v1", box.first[:8])
    assert r.code == 0, r.err
    record = json.loads((box.versions / "v1.json").read_text())
    binary = box.bin_dir / "shiki-server-v1"
    assert record["label"] == "v1"
    assert record["commit"] == box.first  # 短い commit も完全な sha で残す
    assert record["tree_clean"] is True
    assert record["binary_sha256"] == hashlib.sha256(binary.read_bytes()).hexdigest()
    assert record["rustc"] == "rustc 1.99.0 (abeval 2026-01-01)"
    assert record["built_at"].endswith("Z")
    assert set(record) == {
        "label",
        "commit",
        "tree_clean",
        "binary_sha256",
        "runner_sha256",
        "built_at",
        "rustc",
    }
    # ビルドしたのは指定した commit（HEAD ではない）。ランナーも同じ commit のもの。
    assert binary.read_text() == f"fake-server {box.first}\n"
    assert binary.stat().st_mode & 0o111
    runner = box.bin_dir / "shiki-tabular-runner-v1"
    assert runner.read_text() == f"fake-runner {box.first}\n" and runner.stat().st_mode & 0o111
    assert record["runner_sha256"] == hashlib.sha256(runner.read_bytes()).hexdigest()
    wt = box.wt_root / "abeval-v1"
    assert r.log == [
        f"cargo build -p shiki-api --bin shiki-server cwd={wt} target={wt}/target",
        "cargo build --locked --manifest-path crates/tabular/runner/Cargo.toml "
        f"cwd={wt} target={wt}/target/tabular-runner",
    ]
    assert not wt.exists()
    assert box.worktrees() == [str(box.repo)]


def test_dirty_worktree_is_refused(box):
    # checkout の後に未追跡のファイルを作るフック（汚れた worktree の再現）。
    shellkit.write_exe(box.repo / ".git" / "hooks" / "post-checkout", "#!/bin/sh\ntouch junk.txt\n")
    r = box.run("v1", "HEAD")
    assert r.code == 1
    assert "きれいではありません" in r.err and "junk.txt" in r.err
    assert r.log == [], "汚れた worktree でビルドした"
    assert not (box.versions / "v1.json").exists()
    assert not (box.bin_dir / "shiki-server-v1").exists()
    assert not (box.wt_root / "abeval-v1").exists()
    assert box.worktrees() == [str(box.repo)]


def test_build_that_changes_the_tree_is_refused(box):
    r = box.run("v1", "HEAD", FAKE_CARGO_TOUCH="1")
    assert r.code == 1
    assert "書き換えました" in r.err
    assert not (box.versions / "v1.json").exists()
    assert not (box.wt_root / "abeval-v1").exists()


def test_failed_build_removes_the_worktree(box):
    r = box.run("v1", "HEAD", FAKE_CARGO_FAIL="1")
    assert r.code != 0
    assert not (box.versions / "v1.json").exists()
    assert not (box.bin_dir / "shiki-server-v1").exists()
    assert not (box.wt_root / "abeval-v1").exists()
    assert box.worktrees() == [str(box.repo)]


def test_dry_run_creates_nothing(box):
    r = box.run("--dry-run", "v1", "HEAD")
    assert r.code == 0, r.err
    assert box.head in r.out
    assert r.log == []
    for p in (box.wt_root, box.bin_dir, box.versions):
        assert not p.exists()
    assert box.worktrees() == [str(box.repo)]


def test_existing_label_is_refused(box):
    assert box.run("v1", "HEAD").code == 0
    before = (box.versions / "v1.json").read_bytes()
    r = box.run("v1", box.first)
    assert r.code == 2
    assert "既にあります" in r.err
    assert (box.versions / "v1.json").read_bytes() == before


@pytest.mark.parametrize("label", ["V1", "1v", "v_1", "v1-", "a" * 22, "", "v1/x"])
def test_bad_label_is_refused(box, label):
    r = box.run(label, "HEAD")
    assert r.code == 2
    assert r.log == []


def test_unknown_commit_is_refused(box):
    r = box.run("v1", "0" * 40)
    assert r.code == 2
    assert "commit が見つかりません" in r.err


@pytest.mark.parametrize("root", ["/tmp/abeval-wt", "/dev/shm/abeval-wt"])
def test_tmpfs_worktree_root_is_refused(box, root):
    r = shellkit.run_bash(
        BUILD_VERSION,
        ["--repo", str(box.repo), "--worktree-root", root, "--dry-run", "v1", "HEAD"],
        cwd=box.tmp,
        env=box.env,
        log=box.log,
    )
    assert r.code == 2
    assert "tmpfs" in r.err


def test_usage_errors(box):
    assert box.run("v1").code == 2
    assert box.run("v1", "HEAD", "extra").code == 2
    assert box.run("--bin-dir").code == 2
    assert box.run("--nope", "v1", "HEAD").code == 2
    help_ = box.run("--help")
    assert help_.code == 0 and "--worktree-root" in help_.out


def test_syntax():
    p = subprocess.run(
        ["bash", "-n", str(BUILD_VERSION)], capture_output=True, text=True, check=False
    )
    assert p.returncode == 0, p.stderr


def test_a_commit_without_the_runner_is_refused(box):
    box.git("rm", "-q", "-r", "crates")
    box.git("commit", "-q", "-m", "no runner")
    r = box.run("v1", "HEAD")
    assert r.code == 1 and "ランナーのクレートがありません" in r.err
    assert not (box.versions / "v1.json").exists()
    assert not (box.wt_root / "abeval-v1").exists()


def test_stale_runner_lock_is_refused_with_a_hint(box):
    r = box.run("v1", "HEAD", FAKE_CARGO_STALE_LOCK="1")
    assert r.code == 1
    assert "--runner-lock-update" in r.err
    assert not (box.versions / "v1.json").exists()
    assert not (box.versions / "v1.runner-lock.diff").exists()
    assert not (box.bin_dir / "shiki-server-v1").exists()
    assert not (box.wt_root / "abeval-v1").exists()
    assert box.worktrees() == [str(box.repo)]


def test_runner_lock_update_builds_offline_and_records_the_diff(box):
    r = box.run("--runner-lock-update", "v1", box.first[:8], FAKE_CARGO_STALE_LOCK="1")
    assert r.code == 0, r.err
    wt = box.wt_root / "abeval-v1"
    assert r.log == [
        f"cargo build -p shiki-api --bin shiki-server cwd={wt} target={wt}/target",
        "cargo build --offline --manifest-path crates/tabular/runner/Cargo.toml "
        f"cwd={wt} target={wt}/target/tabular-runner",
    ]
    record = json.loads((box.versions / "v1.json").read_text())
    diff = box.versions / "v1.runner-lock.diff"
    assert record["runner_lock_updated"] is True
    assert record["runner_lock_diff_sha256"] == hashlib.sha256(diff.read_bytes()).hexdigest()
    text = diff.read_text()
    assert "crates/tabular/runner/Cargo.lock" in text and '+name = "added-dep"' in text
    # 今ある版は消していない（足しただけ）。
    assert not any(line.startswith("-name") for line in text.splitlines())
    assert record["tree_clean"] is True and record["commit"] == box.first
    assert not wt.exists()


def test_runner_lock_update_without_a_change_records_false(box):
    r = box.run("--runner-lock-update", "v1", "HEAD")
    assert r.code == 0, r.err
    record = json.loads((box.versions / "v1.json").read_text())
    assert record["runner_lock_updated"] is False
    assert "runner_lock_diff_sha256" not in record
    assert not (box.versions / "v1.runner-lock.diff").exists()


def test_runner_lock_update_still_refuses_other_changes(box):
    r = box.run(
        "--runner-lock-update", "v1", "HEAD", FAKE_CARGO_STALE_LOCK="1", FAKE_CARGO_TOUCH="1"
    )
    assert r.code == 1
    assert "書き換えました" in r.err and "README.md" in r.err
    assert "Cargo.lock" not in r.err
    assert not (box.versions / "v1.json").exists()
    assert not (box.versions / "v1.runner-lock.diff").exists()
    assert not (box.wt_root / "abeval-v1").exists()
