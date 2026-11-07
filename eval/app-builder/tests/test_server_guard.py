"""版のサーバーの確かめ（BUG-246・abeval.server_guard と doctor.version_guard）。

偽の /proc（tests/fake_proc.py）で、版のバイナリ・sha・ランナー・LLM の行き先・Mode B の条件・確かめ直しを見る。
本物の /proc と ss は、テストの中で起こした子プロセス（ループバックで待ち受けるだけ）で見る。
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest
from fake_proc import FakeProc

from abeval import doctor, server_guard, versions
from abeval.config import load_config
from abeval.server_guard import Conditions, ServerIdentity

PORT = 10067
API = f"http://localhost:{PORT}"


def eval_llm(path: Path, base: str = "http://127.0.0.1:18200", key: str = "abeval-dummy") -> Path:
    path.write_text(
        '[llm]\nbackend = "stub"\n[[llm.providers]]\n'
        f'id = "anthropic"\nbackend = "anthropic"\nbase_url = "{base}"\napi_key = "{key}"\n'
        'session_header = "x-abeval-session"\n'
        '[[llm.models]]\nid = "claude-haiku-5-5"\nprovider = "anthropic"\n'
        '[[llm.models]]\nid = "default"\nprovider = "stub"\n',
        encoding="utf-8",
    )
    path.chmod(0o600)
    return path


def write_record(label: str, binary: Path, runner: Path | None, **over) -> None:
    raw = {
        "label": label,
        "commit": "c" * 40,
        "tree_clean": True,
        "binary_sha256": versions.file_sha256(binary),
        "built_at": "2026-11-06T00:00:00Z",
        "rustc": "rustc 1.96.0",
    }
    if runner is not None:
        raw["runner_sha256"] = versions.file_sha256(runner)
    raw.update(over)
    versions.write_record(versions.parse_record(raw))


class World:
    """版 v0 の退避したバイナリ・ランナー・記録・設定と、それで動く偽のプロセス。"""

    def __init__(self, tmp_path: Path, monkeypatch):
        self.cfg = load_config()
        home = self.cfg.paths.home
        self.exe = home / "bin" / "shiki-server-v0"
        self.exe.parent.mkdir(parents=True)
        self.exe.write_bytes(b"version server v0\n")
        self.exe.chmod(0o755)
        self.runner = home / "bin" / "shiki-tabular-runner-v0"
        self.runner.write_bytes(b"version runner v0\n")
        self.runner.chmod(0o755)
        write_record("v0", self.exe, self.runner)
        self.server_cfg = eval_llm(tmp_path / "server.toml")
        self.llm = eval_llm(tmp_path / "llm-eval.toml")
        self.proc = FakeProc(tmp_path / "proc")
        monkeypatch.setattr(server_guard, "PROC", self.proc.root)
        monkeypatch.setattr(server_guard, "run_ss", self.proc.ss)
        self.started = time.time() + 5  # 設定のファイルは起動の前に書いた
        self.env = {
            "SHIKI_CONFIG": str(self.server_cfg),
            "SHIKI__TABULAR__RUNNER_PATH": str(self.runner),
            "SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS": "150000",
            "SHIKI__LLM__BACKEND": "stub",
        }
        self.start()

    def start(self, pid: int = 4242, **over) -> int:
        kw = {"exe": self.exe.resolve(), "env": dict(self.env), "started_at": self.started}
        kw.update(over)
        return self.proc.add(pid, **kw)

    def process(self):
        got = server_guard.process_on_port(PORT)
        assert not isinstance(got, str), got
        return got

    def identity(self, label: str = "v0"):
        return {
            c.name: c
            for c in server_guard.identity_checks(self.process(), label, self.cfg.paths.home)
        }

    def guard(self, label: str = "v0", conditions: Conditions | None = None):
        return doctor.version_guard(
            API, self.cfg, label, llm_config=self.llm, conditions=conditions
        )


@pytest.fixture()
def world(tmp_path, monkeypatch):
    return World(tmp_path, monkeypatch)


def ngs(checks) -> list[str]:
    return [c.line() for c in checks if c.status == "ng"]


# --- プロセスの読み取り ---


def test_reads_the_one_process_on_the_port(world):
    p = world.process()
    assert p.pid == 4242 and p.exe == str(world.exe.resolve())
    assert p.env["SHIKI_CONFIG"] == str(world.server_cfg)
    # stat の comm に括弧と空白があっても、最後の ")" の後から起動の時刻を読む。
    assert p.started_at == pytest.approx(world.started, abs=1 / os.sysconf("SC_CLK_TCK") + 1e-6)


def test_unclear_holders_are_reasons_not_processes(world):
    assert "ss がありません" in server_guard.process_on_port(PORT, ss=lambda port: None)
    assert "待ち受けているプロセスがありません" in server_guard.process_on_port(
        PORT, ss=lambda p: ""
    )
    hidden = "LISTEN 0 1024 127.0.0.1:10067 0.0.0.0:*\n"
    assert "別の利用者" in server_guard.process_on_port(PORT, ss=lambda p: hidden)
    world.start(4243)
    world.proc.listening = [4242, 4243]
    assert "1 つではありません" in server_guard.process_on_port(PORT)
    world.proc.listening = [999]
    assert "/proc を読めません" in server_guard.process_on_port(PORT)


# --- 版のバイナリ ---


def test_the_version_binary_passes(world):
    got = world.identity()
    assert [c.status for c in got.values()] == ["ok", "ok"], [c.line() for c in got.values()]


def test_another_binary_on_the_port_is_refused(world, tmp_path):
    head = tmp_path / "repo" / "target" / "debug" / "shiki-server"
    head.parent.mkdir(parents=True)
    head.write_bytes(b"version server v0\n")  # 中身が同じでも、退避したバイナリでなければ NG
    world.start(exe=head)
    c = world.identity()["版のバイナリ"]
    assert c.status == "ng" and "shiki-server" in c.detail and "期待" in c.detail


def test_the_sha_of_the_running_binary_must_match_the_record(world):
    write_record("v0", world.exe, world.runner, binary_sha256="d" * 64)
    c = world.identity()["版のバイナリ"]
    assert c.status == "ng" and "記録 dddddddddddd" in c.detail


def test_a_deleted_binary_is_refused(world):
    world.start(exe=f"{world.exe.resolve()} (deleted)")
    c = world.identity()["版のバイナリ"]
    assert c.status == "ng" and "消された" in c.detail


def test_the_label_is_the_one_checked(world):
    # v1 の記録が無ければ NG（v0 のサーバーを v1 の名前で流さない）。
    assert world.identity("v1")["版の記録"].status == "ng"
    other = world.exe.with_name("shiki-server-v1")
    other.write_bytes(b"version server v1\n")
    write_record("v1", other, world.runner)
    c = world.identity("v1")["版のバイナリ"]
    assert c.status == "ng" and "shiki-server-v1" in c.detail


def test_a_dirty_record_is_refused(world):
    write_record("v0", world.exe, world.runner, tree_clean=False)
    assert world.identity()["版の記録"].status == "ng"


@pytest.mark.parametrize("change", ["head_runner", "sha", "no_record_sha", "no_env"])
def test_the_runner_must_be_the_versions(world, tmp_path, change):
    if change == "head_runner":
        head = tmp_path / "shiki-tabular-runner"
        head.write_bytes(b"version runner v0\n")
        world.start(env={**world.env, "SHIKI__TABULAR__RUNNER_PATH": str(head)})
    elif change == "sha":
        world.runner.write_bytes(b"rebuilt runner\n")
    elif change == "no_record_sha":
        write_record("v0", world.exe, None)
    else:
        env = dict(world.env)
        del env["SHIKI__TABULAR__RUNNER_PATH"]
        world.start(env=env)
    assert world.identity()["版のランナー"].status == "ng"


# --- LLM の行き先 ---


def test_the_route_must_be_the_proxy_with_a_dummy_key(world, tmp_path):
    assert server_guard.route_problem(world.process(), world.cfg) is None
    relay = eval_llm(tmp_path / "relay.toml", "http://127.0.0.1:8787")
    world.start(env={**world.env, "SHIKI_CONFIG": str(relay)})
    assert "abeval.proxy" in server_guard.route_problem(world.process(), world.cfg)
    real = eval_llm(tmp_path / "real.toml", key="not-the-dummy")
    world.start(env={**world.env, "SHIKI_CONFIG": str(real)})
    why = server_guard.route_problem(world.process(), world.cfg)
    assert "ダミーではない" in why and "not-the-dummy" not in why
    world.start(env={**world.env, "SHIKI__LLM__PROVIDERS__0__BASE_URL": "x"})
    assert "上書き" in server_guard.route_problem(world.process(), world.cfg)
    env = {k: v for k, v in world.env.items() if k != "SHIKI_CONFIG"}
    world.start(env=env)
    assert "SHIKI_CONFIG がありません" in server_guard.route_problem(world.process(), world.cfg)


RELAY = "http://127.0.0.1:8787"


def _with_top(path: Path, top: str) -> Path:
    """[llm] の表の先頭（backend の行など）を書き換えた設定（anthropic は abeval.proxy のまま）。"""
    text = path.read_text(encoding="utf-8").replace('[llm]\nbackend = "stub"\n', f"[llm]\n{top}", 1)
    out = path.with_name(f"top-{abs(hash(top))}.toml")
    out.write_text(text, encoding="utf-8")
    return out


@pytest.mark.parametrize(
    ("env", "top", "why"),
    [
        # 既定のプロバイダを環境で anthropic にした（モデル default が手元の中継へ直接届く）。
        ({"SHIKI__LLM__BACKEND": "anthropic"}, None, "SHIKI__LLM__BACKEND"),
        ({"shiki__llm__backend": "anthropic"}, None, "SHIKI__LLM__BACKEND"),
        # 小文字の名前も figment は読む（前置きの比べは大文字・小文字を区別しない）。
        (
            {"shiki__llm__providers": f'[{{id="x",backend="anthropic",base_url="{RELAY}"}}]'},
            None,
            "上書き",
        ),
        # llm の表をまるごと上書きする書き方。
        ({"SHIKI__LLM": f'{{backend="anthropic",base_url="{RELAY}"}}'}, None, "上書き"),
        ({"SHIKI__LLM____X": "1"}, None, "上書き"),  # 空の段より後は入れ子にならない（llm の全体）
        ({"SHIKI__": '{llm={backend="anthropic"}}'}, None, "上書き"),  # 設定の全体
        # 環境に BACKEND が無い: TOML の backend が stub でなければ NG（製品の既定は vllm）。
        ({"SHIKI__LLM__BACKEND": None}, 'backend = "anthropic"\n', "[llm] backend"),
        ({"SHIKI__LLM__BACKEND": None}, "", "[llm] backend"),
        # 最上位の base_url・api_key（campaign の設定から残った手元の中継）。
        (None, f'backend = "stub"\nbase_url = "{RELAY}"\n', "base_url"),
        ({"SHIKI__LLM__BACKEND": None}, f'backend = "stub"\nbase_url = "{RELAY}"\n', "base_url"),
        (None, 'backend = "stub"\napi_key = "dummy-not-shown"\n', "api_key"),
        # anthropic のほかのプロバイダ・ChatGPT プラン。
        (
            None,
            f'backend = "stub"\n[[llm.providers]]\nid = "relay"\nbackend = "openai"\nbase_url = "{RELAY}"\n',
            "ほかのプロバイダ",
        ),
        (None, 'backend = "stub"\n[llm.chatgpt]\nenabled = true\n', "chatgpt"),
    ],
)
def test_the_default_provider_and_env_overrides_are_checked(world, env, top, why):
    """モデル default（AI の IR の llm.invoke・製品の中の呼び出し）の行き先も、環境の上書きも見る（serve を使わずに
    手で起動したときに、評価の台帳を通らず手元の中継へ届く形を通さない）。"""
    e = dict(world.env)
    for k, v in (env or {}).items():
        if v is None:
            e.pop(k, None)
        else:
            e[k] = v
    if top is not None:
        e["SHIKI_CONFIG"] = str(_with_top(world.server_cfg, top))
    world.start(env=e)
    got = server_guard.route_problem(world.process(), world.cfg)
    assert got and why in got, got
    assert "dummy-not-shown" not in got and RELAY not in got


def test_the_default_provider_may_come_from_the_toml(world):
    """環境に SHIKI__LLM__BACKEND が無くても、TOML の [llm] backend が stub なら通る（serve を使わない起動）。"""
    e = {k: v for k, v in world.env.items() if k != "SHIKI__LLM__BACKEND"}
    world.start(env=e)
    assert server_guard.route_problem(world.process(), world.cfg) is None
    assert server_guard.shiki_env_key("Shiki__Llm__Backend") == ["LLM", "BACKEND"]
    assert server_guard.shiki_env_key("SHIKI__LLM____X") == ["LLM"]
    assert server_guard.shiki_env_key("SHIKI_CONFIG") is None


def test_a_config_written_after_the_server_started_is_refused(world):
    os.utime(world.server_cfg, (world.started + 60, world.started + 60))
    why = server_guard.route_problem(world.process(), world.cfg)
    assert why and "起動の後に書き換え" in why
    (world.proc.root / "4242" / "stat").write_text("garbage")
    assert "起動の時刻を読めません" in server_guard.route_problem(world.process(), world.cfg)


def test_a_relative_config_is_read_from_the_servers_directory(world, tmp_path):
    world.start(env={**world.env, "SHIKI_CONFIG": "server.toml"}, cwd=tmp_path)
    assert server_guard.route_problem(world.process(), world.cfg) is None


# --- Mode B の条件 ---


@pytest.mark.parametrize(
    ("env", "max_steps", "budget", "ok"),
    [
        ({}, None, 150_000, True),
        ({"SHIKI__CHAT__MAX_STEPS": "12"}, 12, 150_000, True),
        ({"SHIKI__CHAT__MAX_STEPS": "12"}, None, 150_000, False),  # 印は既定・サーバーは 12
        ({}, 12, 150_000, False),  # 印は 12・サーバーは既定
        ({"SHIKI__CHAT__MAX_STEPS": "8"}, 12, 150_000, False),
        ({}, None, 300_000, False),  # 予算が違う
        ({"SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS": None}, None, 150_000, False),  # 予算が無い
    ],
)
def test_mode_b_conditions_must_match_the_server(world, env, max_steps, budget, ok):
    e = dict(world.env)
    for k, v in env.items():
        if v is None:
            e.pop(k, None)
        else:
            e[k] = v
    world.start(env=e)
    [c] = server_guard.condition_checks(world.process(), Conditions(max_steps, budget))
    assert (c.status == "ok") is ok, c.line()


# --- 組み立て（doctor.version_guard）と確かめ直し ---


def test_version_guard_returns_the_identity_only_when_everything_passes(world):
    checks, ident = world.guard(conditions=Conditions(None, 150_000))
    assert ngs(checks) == [] and isinstance(ident, ServerIdentity)
    assert ident.process.pid == 4242 and ident.recheck() is None
    names = {c.name for c in checks}
    assert {"版のバイナリ", "版のランナー", "版のサーバーの条件", "版のサーバーの設定"} <= names
    # 条件が違えば・別の版の label なら・:8080 なら、確かめたものを返さない。
    _, none = world.guard(conditions=Conditions(12, 150_000))
    assert none is None
    _, none = world.guard("v1")
    assert none is None
    checks, none = doctor.version_guard(
        "http://localhost:8080", world.cfg, "v0", llm_config=world.llm
    )
    assert none is None and any("取り込みの送り先" in line for line in ngs(checks))


def test_recheck_sees_a_restarted_or_changed_server(world):
    _, ident = world.guard(conditions=Conditions(None, 150_000))
    assert ident.recheck() is None
    # 同じ pid でも起動の時刻が違えば別のプロセス（pid の使い回し）。
    world.start(started_at=world.started + 30)
    assert "替わりました" in ident.recheck()
    world.start()
    assert ident.recheck() is None
    world.start(4300)
    assert "pid 4242 → 4300" in ident.recheck()
    world.start(4242)
    os.utime(world.server_cfg, (world.started + 60, world.started + 60))
    assert "LLM の行き先" in ident.recheck()
    world.proc.listening = []
    assert "確かめ直せません" in ident.recheck()


# --- 本物の /proc と ss ---

LISTENER = """
import socket, sys, time
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
print(s.getsockname()[1], flush=True)
time.sleep(120)
"""


@pytest.mark.skipif(shutil.which("ss") is None, reason="ss がありません")
def test_the_real_proc_and_ss(tmp_path, monkeypatch):
    """子プロセス（python）を版のバイナリに見立て、本物の ss と /proc で確かめる。"""
    cfg = load_config()
    real_exe = Path(os.path.realpath(sys.executable))
    home = cfg.paths.home
    (home / "bin").mkdir(parents=True)
    # 退避したバイナリの置き場が、子プロセスの実行ファイルを指すようにする（resolve で同じになる）。
    (home / "bin" / "shiki-server-v0").symlink_to(real_exe)
    runner = home / "bin" / "shiki-tabular-runner-v0"
    runner.write_bytes(b"runner\n")
    write_record("v0", real_exe, runner)
    server_cfg = eval_llm(tmp_path / "server.toml")
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin"),
        "SHIKI_CONFIG": str(server_cfg),
        "SHIKI__TABULAR__RUNNER_PATH": str(runner),
        "SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS": "150000",
    }
    child = subprocess.Popen(
        [str(real_exe), "-I", "-c", LISTENER], stdout=subprocess.PIPE, text=True, env=env
    )
    try:
        port = int(child.stdout.readline())
        proc = server_guard.process_on_port(port)
        assert not isinstance(proc, str), proc
        assert proc.pid == child.pid and proc.exe == str(real_exe)
        assert proc.started_at is not None and abs(proc.started_at - time.time()) < 60
        assert ngs(server_guard.identity_checks(proc, "v0", home)) == []
        assert server_guard.route_problem(proc, cfg) is None
        assert ngs(server_guard.condition_checks(proc, Conditions(None, 150_000))) == []
        write_record("v0", real_exe, runner, binary_sha256="e" * 64)
        assert ngs(server_guard.identity_checks(proc, "v0", home))
        ident = ServerIdentity(port, proc, cfg)
        assert ident.recheck() is None
    finally:
        child.kill()
        child.wait()
    assert "確かめ直せません" in ident.recheck()


STACK = os.environ.get("ABEVAL_API_URL", "").rstrip("/")


@pytest.mark.server
@pytest.mark.skipif(not STACK, reason="ABEVAL_API_URL が無いので飛ばします")
def test_the_running_stack_is_read_from_proc():
    """動いているスタックのサーバーを本物の ss と /proc で読み、そのバイナリを版に見立てて確かめる。"""
    from urllib.parse import urlsplit

    port = urlsplit(STACK).port or 80
    proc = server_guard.process_on_port(port)
    if isinstance(proc, str):
        pytest.skip(f"スタックのサーバーを読めないので飛ばします: {proc}")
    cfg = load_config()
    home = cfg.paths.home
    (home / "bin").mkdir(parents=True)
    exe = Path(proc.exe)
    (home / "bin" / "shiki-server-stack").symlink_to(exe)
    runner = Path(proc.env["SHIKI__TABULAR__RUNNER_PATH"])
    (home / "bin" / "shiki-tabular-runner-stack").symlink_to(runner)
    write_record("stack", exe, runner)
    got = server_guard.identity_checks(proc, "stack", home)
    assert ngs(got) == [], ngs(got)
    assert proc.started_at is not None and proc.started_at < time.time()
    # ほかの版の名前では通らない（このバイナリは v0 の退避したバイナリではない）。
    assert ngs(server_guard.identity_checks(proc, "v0", home))
    ident = ServerIdentity(port, proc, cfg)
    why = ident.recheck()
    # stub のスタックの anthropic が abeval.proxy を指すとき（取り込みの確かめと同じ設定）だけ、行き先も通る。
    assert why is None or "LLM の行き先" in why
