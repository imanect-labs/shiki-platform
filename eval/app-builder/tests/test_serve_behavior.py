"""serve が版のサーバーを起動した直後に振る舞いの指紋を残すこと（§4.1 の 5・BUG-246）。

dev-up.sh は呼ばない（subprocess.run を差し替える）。:10067 を握るプロセスは偽の /proc、検証の口は偽の client。
"""

from __future__ import annotations

import subprocess

import pytest
import test_server_guard as sg
from test_behavior import V0_LIKE, FakeValidator

from abeval import behavior, shiki, versions


class FakeClient(FakeValidator):
    def __init__(self, *a, **k):
        super().__init__(V0_LIKE)
        made.append(self)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


made: list[FakeClient] = []


@pytest.fixture()
def world(tmp_path, monkeypatch):
    made.clear()
    w = sg.World(tmp_path, monkeypatch)
    monkeypatch.setattr(shiki, "ShikiClient", FakeClient)
    calls = []

    def fake_run(argv, **kw):
        calls.append(argv)
        return subprocess.CompletedProcess(argv, 0)

    monkeypatch.setattr(versions.subprocess, "run", fake_run)
    w.dev_up_calls = calls
    return w


def serve(world, *extra) -> int:
    return versions.main(["--label", "v0", "--llm-config", str(world.llm), *extra])


def test_serve_records_the_fingerprint_right_after_starting(world):
    assert serve(world) == 0
    assert len(world.dev_up_calls) == 1 and "--server-bin" in world.dev_up_calls[0]
    rec = versions.read_record("v0")
    assert rec.extra["behavior_check"]["results"] == behavior.fingerprint(FakeValidator(V0_LIKE))
    assert rec.extra["behavior_check"]["probes_sha"] == behavior.PROBES_SHA
    assert len(made) == 1 and len(made[0].sent) == len(behavior.PROBES)


def test_serve_does_not_record_from_a_server_that_is_not_the_version(world, tmp_path, capsys):
    head = tmp_path / "shiki-server"
    head.write_bytes(b"head\n")
    world.start(exe=head)  # 別のバイナリが :10067 を握っていた（起動に失敗して前のが残ったなど）
    assert serve(world) == 1
    assert "behavior_check" not in versions.read_record("v0").extra
    assert made == []
    assert "指紋を残しません" in capsys.readouterr().err


def test_record_behavior_does_not_start_anything(world):
    assert serve(world, "--record-behavior") == 0
    assert world.dev_up_calls == []
    assert "behavior_check" in versions.read_record("v0").extra


def test_down_and_dry_run_do_not_record(world):
    assert serve(world, "--down") == 0
    assert serve(world, "--dry-run") == 0
    assert "behavior_check" not in versions.read_record("v0").extra
    assert made == []


def test_a_failed_start_does_not_record(world, monkeypatch):
    monkeypatch.setattr(
        versions.subprocess, "run", lambda argv, **kw: subprocess.CompletedProcess(argv, 1)
    )
    assert serve(world) == 1
    assert "behavior_check" not in versions.read_record("v0").extra
