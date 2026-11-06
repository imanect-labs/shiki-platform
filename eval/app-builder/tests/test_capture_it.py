"""取り込みを stub のスタックで確かめる（`pytest -m server`・製品の anthropic が abeval.proxy を指すスタックだけ）。

例: ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server tests/test_capture_it.py -q -s

製品の要求を実物で受けるため、発話の model を `claude-haiku-5-5` にする。実 LLM を呼ばないよう、次をすべて
確かめてから送る（どれかが違えば飛ばす）:

1. API のポートを握っているプロセスの `SHIKI_CONFIG`（/proc/<pid>/environ）の anthropic の base_url が
   `http://127.0.0.1:18200`（abeval.proxy）で、api_key がダミー。
2. 127.0.0.1:18200 が空いていて、テストの中で abeval.proxy を起動できる（上流は繋がらない宛先
   `http://127.0.0.1:9`・登録は capture_only だけ）。capture_only は上流へ送らずに最小の応答を返す（$0）。

8787（手元の中継）へは繋がない（conftest の見張り）。
"""

from __future__ import annotations

import os
import re
import socket
import subprocess
import tomllib
from pathlib import Path
from urllib.parse import urlsplit

import pytest

from abeval import cli, fixtures
from abeval.capture import today_note
from abeval.config import load_config
from abeval.ledger import Ledger
from abeval.proxy import ProxyServer, ProxyState
from abeval.shiki import ShikiClient
from abeval.tasks import load_tasks

API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")
PROXY = ("127.0.0.1", 18200)
TASK = {t.id: t for t in load_tasks()}["app01-claim-submit"]

pytestmark = [
    pytest.mark.server,
    pytest.mark.skipif(
        not API, reason="ABEVAL_API_URL が無いので飛ばします（stub のスタックの URL を渡す）"
    ),
]


def server_config(port: int) -> Path | None:
    """そのポートを握っているプロセスの SHIKI_CONFIG（読めなければ None）。"""
    try:
        out = subprocess.run(
            ["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True, check=False
        ).stdout
    except FileNotFoundError:
        return None
    m = re.search(r"pid=(\d+)", out)
    if not m:
        return None
    try:
        env = Path(f"/proc/{m.group(1)}/environ").read_bytes().split(b"\0")
    except OSError:
        return None
    for kv in env:
        if kv.startswith(b"SHIKI_CONFIG="):
            return Path(kv.split(b"=", 1)[1].decode())
    return None


def anthropic_points_to_the_proxy(path: Path) -> str | None:
    """製品の anthropic が abeval.proxy を指し、鍵がダミーか（違えば理由）。値は出さない。"""
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        return f"{path} を読めません: {type(e).__name__}"
    provs = [
        p for p in (raw.get("llm") or {}).get("providers") or [] if p.get("backend") == "anthropic"
    ]
    if len(provs) != 1:
        return f"anthropic のプロバイダが {len(provs)} 個"
    if str(provs[0].get("base_url", "")).rstrip("/") != f"http://{PROXY[0]}:{PROXY[1]}":
        return "anthropic の base_url が abeval.proxy（127.0.0.1:18200）ではない"
    if provs[0].get("api_key") != "abeval-dummy":
        return "anthropic の api_key がダミーではない（値は出さない）"
    return None


def port_free(host: str, port: int) -> bool:
    with socket.socket() as s:
        return s.connect_ex((host, port)) != 0


@pytest.fixture(scope="module")
def guarded():
    port = urlsplit(API).port or 80
    cfg_path = server_config(port)
    if cfg_path is None:
        pytest.skip(f":{port} のサーバーの SHIKI_CONFIG を読めないので飛ばします")
    why = anthropic_points_to_the_proxy(cfg_path)
    if why:
        pytest.skip(f"実 LLM に届くおそれがあるので飛ばします: {why}")
    if not port_free(*PROXY):
        pytest.skip("127.0.0.1:18200 が使われているので飛ばします")
    return cfg_path


def test_capture_from_the_real_product_request(guarded, tmp_path):
    cfg = load_config()
    state = ProxyState(cfg, upstream="http://127.0.0.1:9")
    server = ProxyServer(state, *PROXY)
    state.write_token()
    import threading

    t = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    t.start()
    alice = ShikiClient(
        API, "alice", timeout=60.0
    )  # 発話の model を実モデルの名前にするので stub の守りは外す
    try:
        node, made = fixtures.init_sentinel(alice)
        before = sorted(x["id"] for x in alice.threads())
        with cli.ProxyControl(server.url, state.token) as ctrl:
            cap = cli.do_capture(alice, ctrl, cfg, "itcap", "it", TASK, guarded)
            assert ctrl.spend()["runs"] == {}
        assert sorted(x["id"] for x in alice.threads()) == before
        if made:
            alice.delete_folder(node["id"], missing_ok=True)
    finally:
        server.shutdown()
        server.server_close()
        alice.close()
    names = cap.tool_names
    print(
        f"\n取り込んだ tools: {names}\nmax_tokens={cap.max_tokens} thinking={cap.meta.get('thinking')}"
    )
    print(f"指紋: {cap.meta['fingerprint']}")
    assert "emit_workflow" in names and "read_workflow" in names
    assert "data_record_write" in names  # fixture のテーブルがあるので data のツールが出る
    assert cap.meta["date_note_found"] and cap.meta["original_dates"]
    assert cap.system.endswith(today_note(cfg.capture_date, "Asia/Tokyo"))
    assert cap.meta["thinking"] == {"type": "adaptive"}
    assert cap.meta["output_config"] == {"effort": "medium"}
    data_desc = next(t for t in cap.tools if t["name"] == "data_query")["description"]
    assert "@table:claims" in data_desc and "@table:approvers" in data_desc
    assert "defer_loading" not in cap.emit_tool()
    fp = cap.meta["fingerprint"]
    assert fp["custom_instructions"] == "empty" and "claude-haiku-5-5" in fp["workflow_models"]
    assert fp["capabilities"]["list_workflows"] is True
    entries = Ledger(cfg.paths.ledger).entries()
    assert entries and all(e["captured"] and e["cost_micros"] == 0 for e in entries)
    print(f"保存した要求 {len(entries)} 件（すべて capture_only・$0）")
