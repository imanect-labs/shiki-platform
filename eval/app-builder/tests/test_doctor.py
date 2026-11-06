"""abeval.doctor（各項目の合格と不合格）。偽の shiki・偽の abeval.proxy（MockTransport）で確かめる。"""

from __future__ import annotations

import json
import shutil

import httpx
import pytest

from abeval import doctor, fixtures
from abeval.config import ROOT, load_config
from abeval.ledger import Ledger
from abeval.proxy import CONTROL_HEADER, TOKEN_FILE
from fake_shiki import FakeShiki

EVAL_API = "http://localhost:10067"
STUB_API = "http://localhost:8080"
EXAMPLE = ROOT / "config" / "llm-eval.example.toml"
TOKEN = "t0ken-for-tests"


class World:
    def __init__(self, tmp_path, *, api_port=10067):
        self.cfg = load_config()
        self.fake = FakeShiki(api_port=api_port)
        self.campaign_up = False
        self.proxy_up = True
        self.cfg.paths.home.mkdir(parents=True, exist_ok=True)
        (self.cfg.paths.home / TOKEN_FILE).write_text(TOKEN)
        self.cfg.paths.relay_ledger.parent.mkdir(parents=True, exist_ok=True)
        self.cfg.paths.relay_ledger.write_text(
            json.dumps({"cost_usd": 1.5}) + "\n" + json.dumps({"cost_usd": 0.25}) + "\n"
        )
        Ledger(self.cfg.paths.ledger).append({"cost_micros": 1234, "run_id": "r1"})
        self.llm = tmp_path / "llm-eval.toml"
        shutil.copy(EXAMPLE, self.llm)
        self.llm.chmod(0o600)
        self.spend_calls: list[httpx.Request] = []

    def handle(self, request: httpx.Request) -> httpx.Response:
        host = (request.url.host, request.url.port)
        if host == ("127.0.0.1", 18200):
            self.spend_calls.append(request)
            if not self.proxy_up:
                raise httpx.ConnectError("refused", request=request)
            if request.headers.get(CONTROL_HEADER) != TOKEN:
                return httpx.Response(403)
            return httpx.Response(
                200,
                json={
                    "eval_spent_micros": 1234,
                    "eval_cap_micros": 35_000_000,
                    "reserved_micros": 0,
                },
            )
        if host == ("localhost", 8080) and self.fake.api_port != 8080:
            if not self.campaign_up:
                raise httpx.ConnectError("refused", request=request)
            return httpx.Response(200, text="ok")
        return self.fake.handle(request)

    def run(self, mode="eval", api=EVAL_API, **kw):
        return doctor.run_checks(
            mode=mode,
            api_url=api,
            cfg=self.cfg,
            llm_config=self.llm,
            transport=httpx.MockTransport(self.handle),
            **kw,
        )


def by_name(checks):
    return {c.name: c for c in checks}


def bad(checks):
    return {c.name for c in checks if c.status == "ng"}


@pytest.fixture
def world(tmp_path):
    w = World(tmp_path)
    alice = w.run(init_sentinel=True)  # 番兵を作ってから点検する
    assert by_name(alice)["番兵を作る"].detail.startswith("作りました")
    return w


def test_everything_passes_with_the_example_config(world):
    checks = world.run()
    assert not bad(checks), [c.line() for c in checks if c.status == "ng"]
    assert not [c for c in checks if c.status == "warn"]
    names = by_name(checks)
    for n in (
        "ポート",
        "キャンペーンの組（:8080）",
        "healthz",
        "login alice",
        "login bob",
        "login charlie",
        "llm models",
        "番兵",
        "カスタム指示",
        "機能（OpenAPI）",
        "llm-eval.toml の権限",
        "base_url",
        "api_key",
        "session_header",
        "llm-eval.toml のモデル",
        "anthropic のモデル",
        "abeval.proxy",
        "手元の中継の台帳",
        "評価の台帳",
    ):
        assert n in names, n
    assert names["login charlie"].detail.startswith("org=b-corp")
    assert "$1.7500" in names["手元の中継の台帳"].detail
    assert "$0.0012" in names["評価の台帳"].detail
    # 合言葉を付けて abeval.proxy に問い、手元の中継（8787）には繋がない。
    assert world.spend_calls and world.spend_calls[0].url.path == "/abeval/spend"
    # api.anthropic.com には問わない。
    assert not any(r.url.host == "api.anthropic.com" for r in world.fake.seen)


def test_stub_mode_needs_only_default_and_skips_eval_items(tmp_path):
    w = World(tmp_path, api_port=8080)
    w.fake.models = ["default"]
    w.run(mode="stub", api=STUB_API, init_sentinel=True)
    checks = w.run(mode="stub", api=STUB_API)
    assert not bad(checks)
    names = by_name(checks)
    assert "ポート" not in names and "abeval.proxy" not in names and "base_url" not in names
    assert not w.spend_calls


def test_eval_needs_both_models(world):
    world.fake.models = ["default"]
    assert bad(world.run()) == {"llm models"}


def test_wrong_port(world):
    world.fake.api_port = 8080
    world.campaign_up = True
    names = by_name(world.run(api=STUB_API))
    assert names["ポート"].status == "ng"


def test_campaign_stack_running_is_a_warning(world):
    world.campaign_up = True
    checks = world.run()
    assert by_name(checks)["キャンペーンの組（:8080）"].status == "warn"
    assert not bad(checks)


def test_missing_sentinel(tmp_path):
    w = World(tmp_path)
    checks = w.run()
    assert bad(checks) == {"番兵"}
    assert "--init-sentinel" in by_name(checks)["番兵"].detail
    assert not [n for n in w.fake.nodes.values() if n["name"] == fixtures.SENTINEL_NAME]


def test_custom_instructions(world):
    world.fake.custom_instructions = None
    assert by_name(world.run())["カスタム指示"].detail.startswith("口が無い版")
    world.fake.custom_instructions = {"about": "秘密の自己紹介", "style": "", "tone": "default"}
    checks = world.run()
    assert bad(checks) == {"カスタム指示"}
    assert "秘密の自己紹介" not in by_name(checks)["カスタム指示"].detail
    world.fake.custom_instructions = {"about": " ", "style": "", "tone": "friendly"}
    assert bad(world.run()) == {"カスタム指示"}


def test_wrong_org(world, monkeypatch):
    monkeypatch.setitem(doctor.SEED_ORGS, "charlie", "a-corp")
    assert bad(world.run()) == {"login charlie"}


def test_login_failure_is_reported(world):
    def handler(request):
        if request.url.path.endswith("/login-actions/authenticate") and b"bob" in request.content:
            return httpx.Response(200, text="<html>Invalid username or password.</html>")
        return world.handle(request)

    checks = doctor.run_checks(
        mode="eval",
        api_url=EVAL_API,
        cfg=world.cfg,
        llm_config=world.llm,
        transport=httpx.MockTransport(handler),
    )
    assert bad(checks) == {"login bob"}
    assert "password" not in by_name(checks)["login bob"].detail.replace("username or password", "")


def test_server_down(world):
    def handler(request):
        if request.url.port == 10067:
            raise httpx.ConnectError("refused", request=request)
        return world.handle(request)

    checks = doctor.run_checks(
        mode="eval",
        api_url=EVAL_API,
        cfg=world.cfg,
        llm_config=world.llm,
        transport=httpx.MockTransport(handler),
    )
    assert "healthz" in bad(checks)
    assert "login alice" not in by_name(checks)


@pytest.mark.parametrize(
    ("old", "new", "item"),
    [
        ('base_url = "http://127.0.0.1:18200"', 'base_url = "http://127.0.0.1:8787"', "base_url"),
        ('api_key = "abeval-dummy"', 'api_key = "not-the-dummy-value"', "api_key"),
        ('session_header = "x-abeval-session"', 'session_header = "x-other"', "session_header"),
        (
            'id = "default"\n',
            'id = "default"\n\n[[llm.models]]\nid = "extra-model"\n',
            "llm-eval.toml のモデル",
        ),
        (
            'id = "claude-haiku-5-5"\nprovider = "anthropic"',
            'id = "claude-sonnet-x"\nprovider = "anthropic"',
            "llm-eval.toml のモデル",
        ),
    ],
)
def test_llm_config_items(world, old, new, item):
    text = world.llm.read_text()
    assert old in text
    world.llm.write_text(text.replace(old, new, 1))
    checks = world.run()
    assert item in bad(checks)
    assert "not-the-dummy-value" not in "\n".join(c.line() for c in checks)


def test_model_outside_the_proxy_allowlist(world):
    text = world.llm.read_text().replace(
        'id = "default"\n',
        'id = "default"\n\n[[llm.models]]\nid = "x-sonnet"\nprovider = "anthropic"\n',
        1,
    )
    world.llm.write_text(text)
    assert {"anthropic のモデル", "llm-eval.toml のモデル"} <= bad(world.run())


def test_llm_config_permissions_and_absence(world):
    world.llm.chmod(0o644)
    assert bad(world.run()) == {"llm-eval.toml の権限"}
    world.llm.unlink()
    assert bad(world.run()) == {"llm-eval.toml"}


def test_proxy_items(world):
    (world.cfg.paths.home / TOKEN_FILE).write_text("wrong")
    assert bad(world.run()) == {"abeval.proxy"}
    (world.cfg.paths.home / TOKEN_FILE).unlink()
    assert bad(world.run()) == {"abeval.proxy"}
    (world.cfg.paths.home / TOKEN_FILE).write_text(TOKEN)
    world.proxy_up = False
    assert bad(world.run()) == {"abeval.proxy"}


def test_relay_ledger(world):
    world.cfg.paths.relay_ledger.write_text(json.dumps({"cost_usd": 95.0}) + "\n")
    assert bad(world.run()) == {"手元の中継の台帳"}
    world.cfg.paths.relay_ledger.write_text("{broken\n")
    assert bad(world.run()) == {"手元の中継の台帳"}
    world.cfg.paths.relay_ledger.unlink()
    checks = world.run()
    assert not bad(checks)
    assert by_name(checks)["手元の中継の台帳"].status == "warn"


def test_eval_ledger_exhausted(world):
    Ledger(world.cfg.paths.ledger).append({"cost_micros": 35_000_000})
    assert bad(world.run()) == {"評価の台帳"}


def test_main_exit_codes(world, monkeypatch, capsys):
    real = doctor.run_checks

    def patched(**kw):
        return real(transport=httpx.MockTransport(world.handle), **{**kw, "llm_config": world.llm})

    monkeypatch.setattr(doctor, "run_checks", patched)
    assert doctor.main(["--mode", "eval", "--api-url", EVAL_API]) == 0
    assert "すべて合格" in capsys.readouterr().out
    world.fake.models = ["default"]
    assert doctor.main(["--mode", "eval", "--api-url", EVAL_API]) == 1
    out = capsys.readouterr().out
    assert "[NG]   llm models" in out and "NG 1 件" in out


def test_bad_mode(world):
    with pytest.raises(ValueError):
        world.run(mode="other")
