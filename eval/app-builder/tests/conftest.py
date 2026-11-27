"""テストの見張り。

- 置き場（ABEVAL_HOME・台帳・キャッシュ・手元の中継の台帳・結果・取り込み・版の記録）を、テストごとの一時の場所に向ける。
- socket の接続を見張る。手元の中継（127.0.0.1:8787・本物の鍵を付けて Anthropic へ送る）と、ループバック以外への
  接続は例外にする（実 LLM と外のネットワークを呼ばない）。見張りはセッションの間ずっと掛け、テストの中で
  起こした別スレッド（偽の上流・abeval.proxy）の接続も見る。例外を握りつぶすコードがあっても、試みた記録が
  残っていればそのテストを落とす。
"""

from __future__ import annotations

import pytest

import netguard


def pytest_configure(config):
    netguard.install()


def pytest_unconfigure(config):
    netguard.uninstall()


@pytest.fixture(autouse=True)
def _isolated_paths(tmp_path, monkeypatch):
    home = tmp_path / "abeval-home"
    monkeypatch.setenv("ABEVAL_HOME", str(home))
    monkeypatch.setenv("ABEVAL_LEDGER", str(tmp_path / "ledger" / "spend.jsonl"))
    monkeypatch.setenv("ABEVAL_CACHE_DIR", str(tmp_path / "cache"))
    monkeypatch.setenv("ABEVAL_RELAY_LEDGER", str(tmp_path / "relay" / "anthropic-spend.jsonl"))
    # 結果と取り込みもテストごとの一時の場所に向ける（リポジトリの results/ と data/captures/ に書かない）。
    monkeypatch.setenv("ABEVAL_RESULTS_DIR", str(tmp_path / "results"))
    monkeypatch.setenv("ABEVAL_CAPTURES_DIR", str(tmp_path / "captures"))
    # 版の記録もテストの一時の場所に向ける（リポジトリの data/versions/ に書かない）。
    monkeypatch.setenv("ABEVAL_VERSIONS_DIR", str(tmp_path / "versions"))
    monkeypatch.delenv("ABEVAL_CONFIG", raising=False)
    yield


@pytest.fixture(autouse=True)
def _no_forbidden_connections():
    assert netguard.guard_installed(), "socket の見張りが掛かっていません"
    netguard.take_blocked()
    yield
    tried = netguard.take_blocked()
    if tried:
        pytest.fail(f"許さない接続を試みました: {tried}")
