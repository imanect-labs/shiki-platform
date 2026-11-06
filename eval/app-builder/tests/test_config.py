from __future__ import annotations

from pathlib import Path

import pytest

from abeval.config import (
    DEFAULT_CONFIG,
    ROOT,
    ConfigError,
    find_secret_like,
    is_loopback_host,
    load_config,
    server_config_sha,
)


def test_defaults_match_the_design(tmp_path):
    cfg = load_config()
    assert cfg.source == DEFAULT_CONFIG
    assert (cfg.caps.eval_cap_usd, cfg.caps.run_cap_usd, cfg.caps.global_cap_usd) == (35, 10, 95)
    assert cfg.caps.eval_cap_micros == 35_000_000
    assert cfg.caps.run_cap_micros == 10_000_000
    assert (cfg.proxy.host, cfg.proxy.port) == ("127.0.0.1", 18200)
    assert cfg.proxy.upstream == "http://127.0.0.1:8787"
    assert cfg.proxy.allowed_models == ("claude-haiku-5-5",)
    assert cfg.proxy.session_header == "x-abeval-session"
    assert cfg.proxy.tag_prefix == "abeval"
    assert cfg.api_url == "http://localhost:10067"
    # D-162（H8 (a)）: 既定で却下し、許可リストは空。
    assert cfg.approve_tools == ()
    assert len(cfg.sha) == 64


def test_paths_follow_the_environment(tmp_path):
    cfg = load_config()
    # conftest がテストごとの一時の場所に向けている（手元の台帳とキャッシュに触らない）。
    assert cfg.paths.home == tmp_path / "abeval-home"
    assert cfg.paths.ledger == tmp_path / "ledger" / "spend.jsonl"
    assert cfg.paths.cache == tmp_path / "cache"
    assert cfg.paths.relay_ledger == tmp_path / "relay" / "anthropic-spend.jsonl"


def test_paths_default_relative_to_the_eval_dir():
    cfg = load_config(env={})
    assert cfg.paths.ledger == ROOT / "results" / "spend.jsonl"
    assert cfg.paths.cache == ROOT / "data" / "cache"
    assert cfg.paths.home == Path("~/.local/share/shiki-eval").expanduser()
    assert (
        cfg.paths.relay_ledger
        == Path("~/.local/state/shiki-campaign/anthropic-spend.jsonl").expanduser()
    )


def _write(tmp_path, text: str) -> Path:
    p = tmp_path / "eval.toml"
    p.write_text(text, encoding="utf-8")
    return p


def _base() -> str:
    return DEFAULT_CONFIG.read_text(encoding="utf-8")


@pytest.mark.parametrize(
    "extra",
    [
        '\n[anthropic]\napi_key = "abeval-dummy"\n',
        '\n[extra]\nAuthorization = "Bearer x"\n',
        '\n[extra]\nnote = "sk-ant-api03-zzz"\n',
        '\n[extra]\nclient_secret = "x"\n',
    ],
)
def test_secret_like_items_are_rejected(tmp_path, extra):
    p = _write(tmp_path, _base() + extra)
    with pytest.raises(ConfigError) as e:
        load_config(p)
    # 拒んだ理由に値を出さない。
    assert "zzz" not in str(e.value)
    assert "Bearer" not in str(e.value)


def test_listen_must_be_loopback(tmp_path):
    for bad in ("0.0.0.0:18200", "[::]:18200", "192.168.1.2:18200", "example.com:18200"):
        p = _write(tmp_path, _base().replace('listen = "127.0.0.1:18200"', f'listen = "{bad}"'))
        with pytest.raises(ConfigError):
            load_config(p)


def test_allowed_models_need_a_price(tmp_path):
    text = _base().replace(
        'allowed_models = ["claude-haiku-5-5"]', 'allowed_models = ["claude-haiku-5-5", "x"]'
    )
    with pytest.raises(ConfigError):
        load_config(_write(tmp_path, text))


def test_run_cap_cannot_exceed_eval_cap(tmp_path):
    text = _base().replace("run_cap_usd = 10", "run_cap_usd = 50")
    with pytest.raises(ConfigError):
        load_config(_write(tmp_path, text))


def test_loopback_hosts():
    assert is_loopback_host("127.0.0.1")
    assert is_loopback_host("127.9.0.1")
    assert is_loopback_host("::1")
    assert is_loopback_host("[::1]")
    assert is_loopback_host("localhost")
    for bad in ("0.0.0.0", "::", "10.0.0.1", "api.anthropic.com", ""):
        assert not is_loopback_host(bad), bad


def test_find_secret_like_reports_places_not_values():
    hits = find_secret_like({"a": {"x_api_key": "v"}, "b": ["sk-ant-123"], "c": "ok"})
    assert hits == ["a.x_api_key", "b[0]"]


def test_server_config_sha_ignores_key_items(tmp_path):
    a = tmp_path / "a.toml"
    b = tmp_path / "b.toml"
    a.write_text(
        '[[llm.providers]]\nid = "anthropic"\nbase_url = "http://127.0.0.1:18200"\napi_key = "dummy-1"\n'
    )
    b.write_text(
        '[[llm.providers]]\nid = "anthropic"\nbase_url = "http://127.0.0.1:18200"\napi_key = "dummy-2"\n'
    )
    assert server_config_sha(a) == server_config_sha(b)
    c = tmp_path / "c.toml"
    c.write_text('[[llm.providers]]\nid = "anthropic"\nbase_url = "http://127.0.0.1:8787"\n')
    assert server_config_sha(a) != server_config_sha(c)
