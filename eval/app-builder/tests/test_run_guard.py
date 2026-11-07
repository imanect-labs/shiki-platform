"""run・capture の前の版のサーバーの守り（BUG-246・cli と mode_b の差し込み口）。

- 版のサーバーと確かめられなければ、`--skip-doctor` でも、Mode A・Mode B・`--regrade-from`・計画だけ・`capture` の
  どれも、サーバーにも abeval.proxy にも何も送らずに止まる（偽の /proc で、:10067 を握るのが別のバイナリ）。
- 版のバイナリでも、振る舞いの指紋が記録と違えば、検証の口のほかは叩かずに止まる。
- Mode B の各試行は、発話の前と、採点した試行をキャッシュに置く前に確かめ直す（`ModeBContext.preflight`）。
"""

from __future__ import annotations

import httpx
import pytest
import test_server_guard as sg

from abeval import behavior, cli, mode_b
from abeval.shiki import ShikiClient
from fake_chat import ChatFake, RunScript, text_message
from fake_shiki import FakeShiki
from mode_b_kit import TASKS, make_ctx, spec
from recorded import catalog


@pytest.fixture()
def world(tmp_path, monkeypatch):
    return sg.World(tmp_path, monkeypatch)


@pytest.fixture()
def no_server(monkeypatch):
    """ShikiClient と abeval.proxy の制御の口を作ったら記録する（作られないことを確かめる）。"""
    made = []
    monkeypatch.setattr(cli, "ShikiClient", lambda *a, **k: made.append(("shiki", a)) or None)
    monkeypatch.setattr(cli, "ProxyControl", lambda *a, **k: made.append(("proxy", a)) or None)
    return made


def run_args(world, *extra, mode="b", label="v0"):
    return [
        "--mode", mode, "--label", label, "--exp", "e0", "--tasks", "basic-summary-file",
        "--trials", "1", "--api-url", sg.API, "--llm-config", str(world.llm), *extra,
    ]  # fmt: skip


@pytest.mark.parametrize(
    "extra",
    [
        ("--go", "--skip-doctor"),  # Mode B を流す
        ("--skip-doctor",),  # 計画だけ（版の記録にノードの種類を書かない）
    ],
)
def test_run_refuses_another_binary_even_with_skip_doctor(
    world, no_server, tmp_path, capsys, extra
):
    head = tmp_path / "target" / "debug" / "shiki-server"
    head.parent.mkdir(parents=True)
    head.write_bytes(b"head server\n")
    world.start(exe=head)
    assert cli.main(run_args(world, *extra), command="run") == 1
    assert no_server == []
    out = capsys.readouterr().out
    assert "[NG]   版のバイナリ" in out and "確かめられるまで送りません" in out


def test_mode_a_and_regrade_refuse_a_server_of_another_label(world, no_server, capsys):
    # :10067 は v0 のサーバー。v1 として流す（取り込みと採点し直し）のは止める。
    other = world.exe.with_name("shiki-server-v1")
    other.write_bytes(b"version server v1\n")
    sg.write_record("v1", other, world.runner)
    for extra in (("--go", "--skip-doctor"), ("--regrade-from", "v0", "--skip-doctor")):
        assert cli.main(run_args(world, *extra, mode="a", label="v1"), command="run") == 1
    assert no_server == []
    assert "shiki-server-v1" in capsys.readouterr().out


def test_mode_b_refuses_other_conditions(world, no_server, capsys):
    # サーバーは手数の上限を省いて起動した（製品の既定）。--max-steps 12 の印で流すのは止める。
    assert cli.main(run_args(world, "--go", "--max-steps", "12"), command="run") == 1
    assert no_server == []
    assert "手数の上限" in capsys.readouterr().out
    # 1 発話の予算も同じ。
    assert cli.main(run_args(world, "--go", "--budget-micros", "300000"), command="run") == 1
    assert no_server == []


def test_capture_refuses_a_server_of_another_label(world, no_server):
    other = world.exe.with_name("shiki-server-v1")
    other.write_bytes(b"version server v1\n")
    sg.write_record("v1", other, world.runner)
    argv = ["--label", "v1", "--exp", "e0", "--api-url", sg.API, "--llm-config", str(world.llm)]
    assert cli.main(argv, command="capture") == 1
    assert no_server == []


def test_a_changed_fingerprint_stops_before_anything_but_validate(world, monkeypatch, capsys):
    """版のバイナリでも、振る舞いの指紋が違えば止める（叩くのはログインと検証の口だけ）。"""
    fake = FakeShiki(api_port=sg.PORT)
    seen: list[tuple[str, str]] = []
    inner = fake.transport()

    def handler(req: httpx.Request) -> httpx.Response:
        seen.append((req.method, req.url.path))
        return inner.handle_request(req)

    def client(api, user, stub_only=False, timeout=60.0):
        return ShikiClient(api, user, transport=httpx.MockTransport(handler), timeout=timeout)

    monkeypatch.setattr(cli, "ShikiClient", client)
    monkeypatch.setattr(cli, "load_catalog", catalog)  # ir_lint のバイナリの無い CI でも流す
    # 記録の指紋は V0 らしいもの（誤りなし）だが、いまのサーバーは誤りを返す。
    behavior.record("v0", sg_validator({"port_next": ["ir.unknown_port"]}))
    proxies = []
    monkeypatch.setattr(cli, "ProxyControl", lambda *a, **k: proxies.append(a))
    # FakeShiki の検証はいつも誤りなし → 記録（ir.unknown_port）と違う。
    assert cli.main(run_args(world, "--go", "--skip-doctor"), command="run") == 1
    out = capsys.readouterr().out
    assert "振る舞いの指紋" in out and "port_next" in out
    api_paths = {p for m, p in seen if not p.startswith(("/realms/", "/auth/"))}
    assert ("POST", "/workflows/validate") in seen
    assert api_paths <= {"/workflows/validate", "/me"}, api_paths
    assert proxies == []


def sg_validator(codes):
    from test_behavior import FakeValidator

    return FakeValidator(codes)


# --- Mode B の各試行の確かめ直し ---


class Flaky:
    """n 回目から理由を返す確かめ直し。"""

    def __init__(self, fail_from: int):
        self.calls = 0
        self.fail_from = fail_from

    def __call__(self):
        self.calls += 1
        return "版のサーバーが替わりました" if self.calls >= self.fail_from else None


def done_script() -> RunScript:
    return RunScript(
        events=[{"type": "status", "status": "done"}],
        messages=[text_message("ワークフローは作りませんでした。")],
    )


def test_mode_b_checks_the_server_before_the_utterance():
    fake = ChatFake(scripts=[done_script()])
    ctx = make_ctx(fake)
    ctx.preflight = Flaky(1)
    trial, errors = mode_b.run_trial(ctx, TASKS["basic-summary-file"], spec("basic-summary-file"))
    assert trial is None and errors[0]["kind"] == "server" and errors[0]["stop"] is True
    assert fake.posted == [] and fake.threads == {}
    # 打ち切りの印で、残りの試行は流さない。
    trials, errs = mode_b.run(
        ctx, [spec("basic-summary-file", 0), spec("basic-summary-file", 1)], TASKS
    )
    assert trials == [] and [e["kind"] for e in errs] == ["server", "stopped"]


def test_mode_b_does_not_cache_a_trial_when_the_server_changed_meanwhile():
    fake = ChatFake(scripts=[done_script()])
    ctx = make_ctx(fake)
    ctx.preflight = Flaky(2)  # 発話の前は通り、採点の後に替わっていた
    task, ts = TASKS["basic-summary-file"], spec("basic-summary-file")
    trial, errors = mode_b.run_trial(ctx, task, ts)
    assert len(fake.posted) == 1
    assert trial is None and [e["kind"] for e in errors] == ["server"]
    assert ctx.cache.get(mode_b.key_for(ctx, task, ts)) is None
    # 確かめ直しが通れば、採点してキャッシュに置く（差し込み口が既定の動きを変えない）。
    fake2 = ChatFake(scripts=[done_script()])
    ctx2 = make_ctx(fake2)
    ctx2.preflight = Flaky(99)
    trial, errors = mode_b.run_trial(ctx2, task, ts)
    assert trial is not None and errors == []
    assert ctx2.cache.get(mode_b.key_for(ctx2, task, ts)) is not None
    assert ctx2.preflight.calls == 2
