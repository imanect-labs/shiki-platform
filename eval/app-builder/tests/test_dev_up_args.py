"""dev-up.sh の版のサーバー向けの引数（GAP-30・H3 (a)）。

dev-up.sh はほかのセッションと共有している。引数を省いた起動が前と同じであること（作る run-server.sh が
バイト単位で同じ・shim の呼び出しの記録も同じ・表示も同じ）を、正解の記録（`tests/fixtures/dev_up/`）と比べて
確かめる。正解は、引数を足す前の dev-up.sh（59bbd2eb^ = 0a0567a1）で作った（HEAD と比べると、commit の後は
自分自身と比べることになり何も守らない。CI の深さ 1 の clone でも読めるよう、記録にして置く）。
docker・cargo・curl などは shim で、本物は動かさない（DB もサーバーも作らない）。

dev-up.sh の既定の経路を意図して変えたときだけ、記録を作り直す（差分を見てから commit する）:

    ABEVAL_REGEN_DEV_UP=1 uv run pytest tests/test_dev_up_args.py -k unchanged -q
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import time
from pathlib import Path

import pytest

import shellkit
from shellkit import DEV_UP

pytestmark = [
    pytest.mark.shell,
    pytest.mark.skipif(not shellkit.bash_available(), reason="bash と git が要る"),
]

LABELED = [
    "--db-name",
    "shiki_eval_t1",
    "--store-name",
    "shiki-eval-t1",
    "--bucket",
    "shiki-eval-t1",
    "--port",
    "10067",
    "--gateway-ports",
    "18190,18191",
    "--redis-db",
    "3",
    "--api-only",
    "--run-label",
    "abeval-t1",
]


GOLDEN = Path(__file__).resolve().parent / "fixtures" / "dev_up"
REGEN = os.environ.get("ABEVAL_REGEN_DEV_UP") == "1"
# 記録を作り直すときに流す元の dev-up.sh（既定は今の dev-up.sh。初めの記録は 0a0567a1 の版で作った）。
BASELINE = os.environ.get("ABEVAL_DEV_UP_BASELINE")


def _baseline_text() -> str:
    return Path(BASELINE).read_text(encoding="utf-8") if BASELINE else DEV_UP.read_text("utf-8")


class Box:
    """偽のリポジトリと shim。スクリプトは毎回 `<root>/dev-up.sh` に写して流す（$0 を同じにする）。"""

    def __init__(self, tmp: Path):
        self.tmp = tmp
        self.root = tmp / "repo"
        self.root.mkdir()
        self.log = tmp / "shim.log"
        self.shims = shellkit.make_shims(tmp / "shims")
        self.env = shellkit.base_env(tmp, self.shims, self.log)
        shellkit.git(self.root, "init", "-q", env=self.env)
        (self.root / "deploy" / "compose").mkdir(parents=True)
        (self.root / "deploy" / "compose" / ".env.example").write_text("X=1\n")
        (self.root / "web" / "node_modules").mkdir(parents=True)
        gen = self.root / "web" / "src" / "generated"
        gen.mkdir(parents=True)
        for f in ("api.d.ts", "audit-actions.ts"):
            (gen / f).write_text("//\n")
        sandbox = self.root / "web" / "editor-sandbox"
        (sandbox / "src").mkdir(parents=True)
        (sandbox / "src" / "main.ts").write_text("//\n")
        (sandbox / "build.mjs").write_text("//\n")
        old = time.time() - 3600
        for f in (sandbox / "src" / "main.ts", sandbox / "build.mjs", sandbox / "src"):
            os.utime(f, (old, old))
        (sandbox / "dist").mkdir()
        (sandbox / "dist" / "slide-editor.html").write_text("<!doctype html>\n")
        self.script = self.root / "dev-up.sh"
        uid = os.getuid()
        ck = _cksum(str(self.root).encode())
        self.rundir = Path(self.env["TMPDIR"]) / f"shiki-dev-up-{uid}-{ck}"
        self.fake_bin = shellkit.write_exe(
            tmp / "eval-bin" / "shiki-server-t1", "#!/bin/sh\nexit 0\n"
        )

    def labeled_rundir(self, label: str) -> Path:
        return Path(f"{self.rundir}-{label}")

    def run(self, text: str, args: list[str], **env) -> shellkit.Run:
        self.script.write_text(text, encoding="utf-8")
        try:
            return shellkit.run_bash(
                self.script, args, cwd=self.root, env={**self.env, **env}, log=self.log
            )
        finally:
            shellkit.kill_pidfiles(self.rundir, *self.rundir.parent.glob(self.rundir.name + "-*"))

    def reset_rundirs(self) -> None:
        for d in [self.rundir, *self.rundir.parent.glob(self.rundir.name + "-*")]:
            shutil.rmtree(d, ignore_errors=True)


def _cksum(data: bytes) -> int:
    """POSIX の cksum（CRC-32/CKSUM・長さを後ろに足す）。"""
    crc = 0
    table = []
    for i in range(256):
        c = i << 24
        for _ in range(8):
            c = ((c << 1) ^ 0x04C11DB7) if c & 0x80000000 else (c << 1)
        table.append(c & 0xFFFFFFFF)
    for b in data:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ table[((crc >> 24) ^ b) & 0xFF]
    n = len(data)
    while n:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ table[((crc >> 24) ^ (n & 0xFF)) & 0xFF]
        n >>= 8
    return (~crc) & 0xFFFFFFFF


def test_cksum_matches_the_command():
    # RUNDIR の名前を同じ式で組むため、cksum の写しが本物と同じことを先に確かめる。
    p = subprocess.run(["cksum"], input=b"/a/b/c", capture_output=True, check=True)
    assert int(p.stdout.split()[0]) == _cksum(b"/a/b/c")


@pytest.fixture
def box(tmp_path):
    b = Box(tmp_path)
    yield b
    shellkit.kill_pidfiles(b.rundir, *b.rundir.parent.glob(b.rundir.name + "-*"))


def _env_lines(text: str) -> dict[str, str]:
    """run-server.sh の行を「キー → 行」に（export の名前・exec・その他の行は行そのもの）。"""
    out = {}
    for line in text.splitlines():
        m = re.match(r"^export ([A-Z0-9_]+)=", line)
        key = m.group(1) if m else ("exec" if line.startswith("exec ") else line)
        out[key] = line
    return out


def _default_run(box: Box, text: str) -> tuple[shellkit.Run, bytes]:
    box.reset_rundirs()
    r = box.run(text, [])
    assert r.code == 0, r.err
    return r, (box.rundir / "run-server.sh").read_bytes()


def _normalize(box: Box, text: str) -> str:
    """一時の場所（実行ごとに違う）を印にする。RUNDIR は root の cksum を含むので先に置き換える。"""
    return text.replace(str(box.rundir), "<RUNDIR>").replace(str(box.tmp), "<TMP>")


def _snapshot(box: Box, r: shellkit.Run, script: bytes | None = None) -> dict:
    snap = {
        "code": r.code,
        "out": _normalize(box, r.out),
        "err": _normalize(box, r.err),
        "log": [_normalize(box, line) for line in r.log],
    }
    if script is not None:
        snap["run_server"] = _normalize(box, script.decode())
    return snap


def _check_golden(name: str, snap: dict) -> None:
    path = GOLDEN / f"{name}.json"
    if REGEN:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(snap, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
        return
    want = json.loads(path.read_text(encoding="utf-8"))
    for key in want:
        assert snap[key] == want[key], f"{name}: {key} が引数を足す前の dev-up.sh と違います"
    assert set(snap) == set(want)


def test_no_args_is_unchanged_from_the_baseline(box):
    """引数を省いた起動は、引数を足す前の dev-up.sh とバイト単位で同じ run-server.sh を作り、同じ shim を同じ順に
    呼び、同じ表示をする（正解の記録と比べる）。"""
    new_run, new_script = _default_run(
        box, _baseline_text() if REGEN else DEV_UP.read_text("utf-8")
    )
    _check_golden("default", _snapshot(box, new_run, new_script))
    # 既定の経路は今までどおり: ビルドする・pgrep で残りを止める・web を起動する。
    assert "cargo build -p shiki-api --bin shiki-server" in new_run.log
    assert any(line.startswith("pgrep -f ^") for line in new_run.log)
    assert any("run-web.sh" in line for line in new_run.log if line.startswith("setsid"))
    assert any("pnpm" in line or "localhost:3000" in line for line in new_run.log)
    text = new_script.decode()
    assert "export SHIKI__DATABASE__URL=postgres://postgres:postgres@localhost:5432/shiki\n" in text
    assert "export SHIKI__AUTHZ__STORE_NAME=shiki\n" in text
    assert "export SHIKI__AUTH__REDIRECT_URI=http://localhost:3000/auth/callback\n" in text
    assert f'exec "{box.root}/target/debug/shiki-server"\n' in text


def test_status_and_down_are_unchanged_from_the_baseline(box):
    for args in (["--status"], ["--down"]):
        box.reset_rundirs()
        r = box.run(_baseline_text() if REGEN else DEV_UP.read_text(encoding="utf-8"), args)
        _check_golden(args[0].lstrip("-"), _snapshot(box, r))


def test_the_golden_records_hold_no_machine_paths():
    """記録に、この機械の一時の場所や利用者名の入ったパスが残らない（印に置き換えてある）。"""
    names = sorted(p.name for p in GOLDEN.glob("*.json"))
    assert names == ["default.json", "down.json", "status.json"]
    for name in names:
        text = (GOLDEN / name).read_text(encoding="utf-8")
        assert "/home/" not in text and ".pytest_tmp" not in text, name
    assert "<TMP>/repo" in (GOLDEN / "default.json").read_text(encoding="utf-8")


def test_labeled_args_change_only_their_lines(box):
    _, default_script = _default_run(box, DEV_UP.read_text(encoding="utf-8"))
    box.reset_rundirs()
    r = box.run(DEV_UP.read_text(encoding="utf-8"), ["--server-bin", str(box.fake_bin), *LABELED])
    assert r.code == 0, r.err
    rundir = box.labeled_rundir("abeval-t1")
    labeled = (rundir / "run-server.sh").read_text()
    before, after = _env_lines(default_script.decode()), _env_lines(labeled)
    assert before.keys() == after.keys()
    changed = {k for k in before if before[k] != after[k]}
    assert changed == {
        "SHIKI__SERVER__PORT",
        "SHIKI__DATABASE__URL",
        "SHIKI__AUTH__REDIRECT_URI",
        "SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI",
        "SHIKI__SESSION__REDIS_URL",
        "SHIKI__AUTHZ__STORE_NAME",
        "SHIKI__STORAGE__S3__BUCKET",
        "SHIKI__GATEWAY__PORT",
        "SHIKI__GATEWAY__B1_PORT",
        "SHIKI__GATEWAY__PUBLIC_ORIGIN",
        "SHIKI__RAG__INDEX_DATA_DIR",  # RUNDIR がラベルごとに分かれるため
        "exec",
    }
    assert after["SHIKI__SERVER__PORT"] == "export SHIKI__SERVER__PORT=10067"
    assert after["SHIKI__DATABASE__URL"].endswith("@localhost:5432/shiki_eval_t1")
    assert after["SHIKI__AUTH__REDIRECT_URI"].endswith("=http://localhost:10067/auth/callback")
    assert after["SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI"].endswith("=http://localhost:10067/")
    assert (
        after["SHIKI__SESSION__REDIS_URL"]
        == "export SHIKI__SESSION__REDIS_URL=redis://localhost:6379/3"
    )
    assert after["SHIKI__AUTHZ__STORE_NAME"] == "export SHIKI__AUTHZ__STORE_NAME=shiki-eval-t1"
    assert after["SHIKI__STORAGE__S3__BUCKET"] == "export SHIKI__STORAGE__S3__BUCKET=shiki-eval-t1"
    assert after["SHIKI__GATEWAY__PORT"] == "export SHIKI__GATEWAY__PORT=18190"
    assert after["SHIKI__GATEWAY__B1_PORT"] == "export SHIKI__GATEWAY__B1_PORT=18191"
    assert after["exec"] == f'exec "{box.fake_bin}"'
    assert str(rundir) in after["SHIKI__RAG__INDEX_DATA_DIR"]
    # DB を作り、印のコメントもその DB に付ける。既定の DB には触れない。
    log = "\n".join(r.log)
    assert "datname = 'shiki_eval_t1'" in log
    assert "CREATE DATABASE shiki_eval_t1;" in log
    assert "-d shiki_eval_t1 -tAc" in log
    assert "COMMENT ON DATABASE shiki_eval_t1 IS 'shiki-dev-store=local/shiki-eval-t1'" in log
    assert "-d shiki " not in log and "DATABASE shiki " not in log
    # ラベルのサーバーの記録（--down が読む）。
    assert (rundir / "server.env").read_text() == f"port=10067\nexe={box.fake_bin}\n"
    assert (rundir / "shiki-server.pid").exists()
    assert not box.rundir.exists()


def test_db_that_exists_is_not_created(box):
    r = box.run(
        DEV_UP.read_text(encoding="utf-8"),
        ["--server-bin", str(box.fake_bin), *LABELED],
        FAKE_DB_EXISTS="1",
    )
    assert r.code == 0, r.err
    assert not any("CREATE DATABASE" in line for line in r.log)


def test_api_only_does_not_start_the_web(box):
    r = box.run(DEV_UP.read_text(encoding="utf-8"), ["--server-bin", str(box.fake_bin), *LABELED])
    assert r.code == 0, r.err
    assert not any(line.startswith(("cargo", "pnpm", "pgrep")) for line in r.log)
    assert not any("localhost:3000" in line for line in r.log)
    starts = [line for line in r.log if line.startswith("setsid")]
    assert len(starts) == 1 and "abeval-t1/run-server.sh" in starts[0]
    assert "curl -fsS --max-time 2 http://localhost:10067/healthz" in r.log
    assert not box.labeled_rundir("abeval-t1").joinpath("run-web.sh").exists()
    assert "web は起動していません" in r.out


def test_api_only_without_port_uses_8080_and_builds(box):
    r = box.run(DEV_UP.read_text(encoding="utf-8"), ["--api-only"])
    assert r.code == 0, r.err
    text = (box.rundir / "run-server.sh").read_text()
    assert "export SHIKI__SERVER__PORT=8080\n" in text
    assert "export SHIKI__AUTH__REDIRECT_URI=http://localhost:8080/auth/callback\n" in text
    assert "cargo build -p shiki-api --bin shiki-server" in r.log
    # ラベルも --server-bin も無ければ、残りのサーバーは今までどおり pgrep で止める。
    assert any(line.startswith("pgrep") for line in r.log)
    assert not any("run-web.sh" in line for line in r.log)


def test_run_label_separates_the_rundir_and_skips_pgrep(box):
    r = box.run(DEV_UP.read_text(encoding="utf-8"), ["--run-label", "abeval-x", "--api-only"])
    assert r.code == 0, r.err
    rundir = box.labeled_rundir("abeval-x")
    assert (rundir / "run-server.sh").exists()
    assert not any(line.startswith("pgrep") for line in r.log)
    # バイナリは既定（cargo build したもの）で、port と exe を記録する。
    exe = box.root / "target" / "debug" / "shiki-server"
    assert (rundir / "server.env").read_text() == f"port=8080\nexe={exe}\n"


def _sleeper() -> subprocess.Popen:
    return subprocess.Popen(["sleep", "60"], start_new_session=True)


def test_down_with_run_label_stops_only_that_label(box):
    rundir = box.labeled_rundir("abeval-t1")
    rundir.mkdir(parents=True)
    box.rundir.mkdir(parents=True)
    mine, other = _sleeper(), _sleeper()
    try:
        (rundir / "shiki-server.pid").write_text(f"{mine.pid}\n")
        (rundir / "server.env").write_text(f"port=10067\nexe={box.fake_bin}\n")
        (box.rundir / "shiki-server.pid").write_text(f"{other.pid}\n")
        r = shellkit.run_bash(
            _write(box, DEV_UP.read_text(encoding="utf-8")),
            ["--down", "--run-label", "abeval-t1"],
            cwd=box.root,
            env=box.env,
            log=box.log,
        )
        assert r.code == 0, r.err
        assert mine.wait(timeout=10) is not None
        assert other.poll() is None, "ラベルの無いサーバーまで止めた"
        assert (box.rundir / "shiki-server.pid").exists()
        assert not (rundir / "shiki-server.pid").exists()
        assert not (rundir / "server.env").exists()
        assert not any(line.startswith("pgrep") for line in r.log)
        # 自分のポートの持ち主だけを見る（:3000 や :8080 には触れない）。
        assert [line for line in r.log if line.startswith("fuser")] == ["fuser 10067/tcp"]
    finally:
        for p in (mine, other):
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()


def _write(box: Box, text: str) -> Path:
    box.script.write_text(text, encoding="utf-8")
    return box.script


@pytest.mark.parametrize(
    "args",
    [
        ["--port", "10067"],
        ["--port", "9999", "--api-only"],
        ["--port", "", "--api-only"],
        ["--db-name"],
        ["--db-name", "bad;drop"],
        ["--db-name", "Shiki"],
        ["--store-name", "a b"],
        ["--bucket", "UPPER"],
        ["--bucket", "ab"],
        ["--bucket", "shiki-eval-x", "--rag-remote"],
        ["--redis-db", "16"],
        ["--redis-db", "x"],
        ["--gateway-ports", "18190"],
        ["--gateway-ports", "18190,18190"],
        ["--gateway-ports", "80,81"],
        ["--run-label", "Bad"],
        ["--server-bin", "relative/shiki-server"],
        ["--server-bin", "/nonexistent/shiki-server"],
        ["--compose", "--api-only"],
        ["--compose", "--db-name", "shiki_eval_x"],
    ],
)
def test_bad_args_exit_2_before_doing_anything(box, args):
    r = box.run(DEV_UP.read_text(encoding="utf-8"), args)
    assert r.code == 2, (r.out, r.err)
    assert r.log == [], "検査の前に何かを呼んだ"
    assert not box.rundir.exists()


def test_help_lists_the_new_args(box):
    r = box.run(DEV_UP.read_text(encoding="utf-8"), ["--help"])
    assert r.code == 0
    for opt in (
        "--server-bin",
        "--db-name",
        "--store-name",
        "--bucket",
        "--redis-db",
        "--port",
        "--gateway-ports",
        "--api-only",
        "--run-label",
    ):
        assert opt in r.out
    assert r.out.rstrip().endswith("dev-up.sh --down --run-label abeval-v0")


def test_syntax():
    p = subprocess.run(["bash", "-n", str(DEV_UP)], capture_output=True, text=True, check=False)
    assert p.returncode == 0, p.stderr


def test_abeval_serve_args_are_accepted_by_dev_up(box, tmp_path):
    """abeval.versions が組む引数を dev-up.sh がそのまま受け付ける（名前の決まりが食い違わない）。"""
    from abeval import versions

    home = tmp_path / "eval-home"
    exe = shellkit.write_exe(home / "bin" / "shiki-server-v0-retry", "#!/bin/sh\nexit 0\n")
    runner = shellkit.write_exe(home / "bin" / "shiki-tabular-runner-v0-retry", "#!/bin/sh\n")
    vdir = tmp_path / "versions"
    versions.write_record(
        versions.parse_record(
            {
                "label": "v0-retry",
                "commit": "0" * 40,
                "tree_clean": True,
                "binary_sha256": versions.file_sha256(exe),
                "runner_sha256": versions.file_sha256(runner),
                "built_at": "2026-11-06T00:00:00Z",
                "rustc": "rustc 1.99.0",
            }
        ),
        vdir,
    )
    cfg = tmp_path / "llm-eval.toml"
    cfg.write_text("[llm]\n")
    cfg.chmod(0o600)
    plan = versions.serve_plan(
        "v0-retry", home=home, llm_config=cfg, versions_dir=vdir, base_env=box.env
    )
    r = box.run(DEV_UP.read_text(encoding="utf-8"), plan.argv[2:], **plan.env)
    assert r.code == 0, r.err
    rundir = box.labeled_rundir("abeval-v0-retry")
    text = (rundir / "run-server.sh").read_text()
    assert "/shiki_eval_v0_retry\n" in text
    assert "export SHIKI__AUTHZ__STORE_NAME=shiki-eval-v0-retry\n" in text
    assert f'exec "{exe.resolve()}"\n' in text
    # 版と同じ commit のランナーを使う（dev-up は呼び出し側の値を使う）。
    assert f"export SHIKI__TABULAR__RUNNER_PATH={runner.resolve()}\n" in text
    down = versions.down_plan("v0-retry", base_env=box.env)
    r = box.run(DEV_UP.read_text(encoding="utf-8"), down.argv[2:], **down.env)
    assert r.code == 0, r.err
    assert not (rundir / "server.env").exists()


def test_down_kills_its_own_server_that_ignores_term(tmp_path):
    """--run-label の停止は、TERM で止まらない自分のサーバーを KILL まで進めて止める（ポートを空ける）。"""
    stubborn = subprocess.Popen(
        ["/usr/bin/bash", "-c", "trap '' TERM; while :; do /usr/bin/sleep 0.2; done"],
        start_new_session=True,
    )
    fuser = '#!/usr/bin/env bash\nprintf \'fuser %s\\n\' "$*" >> "$SHIM_LOG"\n'
    fuser += f"kill -0 {stubborn.pid} 2>/dev/null && printf ' {stubborn.pid}'\nexit 0\n"
    fast_sleep = "#!/usr/bin/env bash\nexec /usr/bin/sleep 0.05\n"
    shims = shellkit.make_shims(tmp_path / "shims2", {"fuser": fuser, "sleep": fast_sleep})
    (tmp_path / "b").mkdir()
    box = Box(tmp_path / "b")
    env = {**box.env, "PATH": f"{shims}:/usr/bin:/bin"}
    rundir = box.labeled_rundir("abeval-t1")
    rundir.mkdir(parents=True)
    exe = os.path.realpath("/usr/bin/bash")
    (rundir / "server.env").write_text(f"port=10067\nexe={exe}\n")
    try:
        r = shellkit.run_bash(
            _write(box, DEV_UP.read_text(encoding="utf-8")),
            ["--down", "--run-label", "abeval-t1"],
            cwd=box.root,
            env=env,
            log=box.log,
        )
        assert r.code == 0, r.err
        assert stubborn.wait(timeout=10) == -signal.SIGKILL
        assert "KILL します" in r.err
        assert r.log.count("fuser 10067/tcp") >= 21  # TERM を 20 回送ってから KILL
    finally:
        if stubborn.poll() is None:
            os.killpg(stubborn.pid, signal.SIGKILL)
            stubborn.wait()
