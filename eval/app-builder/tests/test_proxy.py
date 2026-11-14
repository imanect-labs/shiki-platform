"""abeval.proxy を偽の上流で確かめる（手元の中継 8787 には一度も繋がない・conftest の見張り）。"""

from __future__ import annotations

import json
import os
import socket
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass

import httpx
import pytest

from abeval.config import load_config
from abeval.ledger import Ledger
from abeval.proxy import CONTROL_HEADER, DUMMY_CREDENTIAL, ProxyServer, ProxyState
from abeval.sse import SseReader
from fake_upstream import FakeUpstream, Received, Reply, sse, sse_reply, standard_events

MODEL = "claude-haiku-5-5"
CLIENT_SECRET = "client-secret-xyz"


@dataclass
class Proxy:
    url: str
    state: ProxyState

    def control(self, method: str, path: str, body: object = None, token: str | None = "ok"):
        headers = {}
        if token == "ok":
            headers[CONTROL_HEADER] = self.state.token
        elif token is not None:
            headers[CONTROL_HEADER] = token
        with httpx.Client(trust_env=False, timeout=10) as c:
            return c.request(method, self.url + path, json=body, headers=headers)

    def run(self, run_id="r1", exp="e0", cap_usd=10.0, capture_only=False, active=True, trial="t1"):
        r = self.control(
            "POST",
            "/abeval/runs",
            {"run_id": run_id, "exp": exp, "cap_usd": cap_usd, "capture_only": capture_only},
        )
        assert r.status_code == 201, r.text
        if active:
            r2 = self.control("PUT", "/abeval/active", {"run_id": run_id, "trial": trial})
            assert r2.status_code == 200, r2.text
        return r.json()


@contextmanager
def running_proxy(upstream_url: str, capture: bool = False):
    cfg = load_config()
    state = ProxyState(cfg, upstream=upstream_url, capture=capture)
    server = ProxyServer(state, "127.0.0.1", 0)
    # テストでは応答のスレッドの終わりを待って閉じる（daemon のままだと、テストの後も動くスレッドがインタプリタの
    # 終了と重なって異常終了することがある）。
    server.daemon_threads = False
    server.block_on_close = True
    state.write_token()
    t = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    t.start()
    try:
        yield Proxy(server.url, state)
    finally:
        server.shutdown()
        server.server_close()


def body(**over) -> dict:
    b = {
        "model": MODEL,
        "max_tokens": 1024,
        "stream": True,
        "system": "あなたは業務の自動化を手伝います。",
        "messages": [{"role": "user", "content": "経費の申請を受け付けるフローを作って"}],
    }
    b.update(over)
    return {k: v for k, v in b.items() if v is not None}


def headers(**extra) -> dict:
    h = {
        "x-api-key": CLIENT_SECRET,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
    }
    h.update(extra)
    return h


def post(proxy: Proxy, payload: dict, path="/v1/messages", hdrs=None) -> tuple[int, dict, bytes]:
    raw = json.dumps(payload, ensure_ascii=False).encode()
    with (
        httpx.Client(trust_env=False, timeout=30) as c,
        c.stream("POST", proxy.url + path, content=raw, headers=hdrs or headers()) as r,
    ):
        data = b"".join(r.iter_raw())
        return r.status_code, dict(r.headers), data


def ledger_entries(proxy: Proxy) -> list[dict]:
    return Ledger(proxy.state.cfg.paths.ledger).entries()


@pytest.fixture
def fake():
    with FakeUpstream() as f:
        yield f


def test_default_upstream_is_the_local_relay_but_tests_override_it(fake):
    cfg = load_config()
    assert ProxyState(cfg).upstream == "http://127.0.0.1:8787"
    with running_proxy(fake.url) as p:
        assert p.state.upstream == fake.url
        assert not p.state.upstream.endswith(":8787")


def test_sse_reaches_the_client_byte_for_byte_without_buffering():
    chunks = sse(*standard_events())
    # 行の途中で切れたチャンクも、そのまま届く。
    chunks = [chunks[0][:17], chunks[0][17:], *chunks[1:]]
    reply = Reply(
        headers={"content-type": "text/event-stream", "request-id": "req_abc"},
        chunks=chunks,
        delay=0.25,
    )
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        raw = json.dumps(body()).encode()
        arrivals: list[float] = []
        got = bytearray()
        started = time.monotonic()
        with (
            httpx.Client(trust_env=False, timeout=30) as c,
            c.stream("POST", p.url + "/v1/messages", content=raw, headers=headers()) as r,
        ):
            assert r.status_code == 200
            assert r.headers["content-type"] == "text/event-stream"
            assert r.headers["request-id"] == "req_abc"
            for chunk in r.iter_raw():
                arrivals.append(time.monotonic() - started)
                got.extend(chunk)
        assert bytes(got) == b"".join(chunks)
        # 最初のチャンクは、上流が最後のチャンクを書く（約 1.75 秒後）よりずっと前に届く。
        assert arrivals[0] < 1.0, arrivals
        assert f.received[0].body == raw


def test_client_credentials_and_session_never_reach_upstream(fake):
    with running_proxy(fake.url) as p:
        p.run(active=False)
        r = p.control(
            "POST", "/abeval/sessions", {"session": "thread-abc", "trial": "t7", "run_id": "r1"}
        )
        assert r.status_code == 201, r.text
        status, _, _ = post(
            p,
            body(),
            hdrs=headers(
                **{
                    "authorization": f"Bearer {CLIENT_SECRET}",
                    "x-abeval-session": "thread-abc",
                    "anthropic-beta": "tools-2024-05-16",
                    "accept": "text/event-stream",
                    "user-agent": f"shiki {CLIENT_SECRET}",
                    "x-campaign-tag": "forged",
                }
            ),
        )
        assert status == 200
        got = fake.received[0].headers
        assert got["x-api-key"] == DUMMY_CREDENTIAL
        assert "authorization" not in got
        assert "x-abeval-session" not in got
        assert got["x-campaign-tag"] == "abeval/e0/r1/t7"
        assert got["anthropic-version"] == "2023-06-01"
        assert got["anthropic-beta"] == "tools-2024-05-16"
        assert got["content-type"] == "application/json"
        assert got["accept"] == "text/event-stream"
        assert got["accept-encoding"] == "identity"
        assert got["connection"] == "close"
        assert all(CLIENT_SECRET not in v for v in got.values()), got
        [entry] = ledger_entries(p)
        assert (entry["run_id"], entry["trial"], entry["session"]) == ("r1", "t7", "thread-abc")


def test_unregistered_session_falls_back_to_active(fake):
    with running_proxy(fake.url) as p:
        p.run(run_id="r1", trial="t1")
        p.run(run_id="r2", active=False)
        p.control("POST", "/abeval/sessions", {"session": "s-2", "trial": "t9", "run_id": "r2"})
        post(p, body(), hdrs=headers(**{"x-abeval-session": "s-unknown"}))
        post(p, body(), hdrs=headers(**{"x-abeval-session": "S-2"}))
        tags = [r.headers["x-campaign-tag"] for r in fake.received]
        assert tags == ["abeval/e0/r1/t1", "abeval/e0/r2/t9"]


@pytest.mark.parametrize(
    "payload, needle",
    [
        (body(model="claude-opus-5-5"), "許可リスト"),
        (body(temperature=0.7), "temperature"),
        (body(top_p=0.9), "top_p"),
        (body(top_k=5), "top_k"),
        (body(max_tokens=None), "max_tokens"),
    ],
)
def test_rejected_before_upstream(fake, payload, needle):
    with running_proxy(fake.url) as p:
        p.run()
        status, _, data = post(p, payload)
        assert status == 400
        err = json.loads(data)
        assert err["type"] == "error"
        assert err["error"]["type"] == "invalid_request_error"
        assert err["error"]["message"].startswith("abeval: ")
        assert needle in err["error"]["message"]
        assert fake.received == []
        assert ledger_entries(p) == []


def test_unregistered_run_is_rejected(fake):
    with running_proxy(fake.url) as p:
        status, _, data = post(p, body())
        assert status == 400
        assert "run" in json.loads(data)["error"]["message"]
        # 登録しても active にも会話 ID にも割り当てなければ拒む。
        p.run(active=False)
        status, _, _ = post(p, body(), hdrs=headers(**{"x-abeval-session": "unknown"}))
        assert status == 400
        # 消した run には割り当たらない。
        p.run(run_id="r9")
        assert p.control("DELETE", "/abeval/runs/r9").status_code == 204
        status, _, _ = post(p, body())
        assert status == 400
        assert fake.received == []


def _prefill(path, *entries: dict) -> None:
    led = Ledger(path)
    for e in entries:
        led.append(e)


def test_eval_cap_is_enforced_before_sending(fake):
    cfg = load_config()
    cap = cfg.caps.eval_cap_micros
    _prefill(
        cfg.paths.ledger,
        {"ts": "2026-10-31T10:00:00+0900", "run_id": "old", "cost_micros": cap - 2_000},
        {"ts": "2026-11-01T10:00:00+0900", "run_id": "older", "cost_micros": 1_000},
    )
    with running_proxy(fake.url) as p:
        p.run()
        # 残り 1,000 マイクロ。max_tokens 1024 の最悪（≒ 540）は通り、8192（≒ 4,130）は通らない。
        small = body(max_tokens=1024)
        raw_len = len(json.dumps(small).encode())
        assert cfg.pricing.reserve_micros(MODEL, raw_len, 1024) < 1_000
        status, _, data = post(p, body(max_tokens=8192))
        assert status == 400, data
        assert "評価の予算" in json.loads(data)["error"]["message"]
        assert fake.received == []
        status, _, _ = post(p, small)
        assert status == 200
        assert len(fake.received) == 1


def test_run_cap_is_enforced_before_sending(fake):
    cfg = load_config()
    _prefill(cfg.paths.ledger, {"run_id": "r1", "cost_micros": 9_999_000})
    with running_proxy(fake.url) as p:
        # 登録した額が $10 を超えても、上限は $10。
        info = p.run(cap_usd=50)
        assert info["cap_micros"] == 10_000_000
        assert info["spent_micros"] == 9_999_000
        status, _, data = post(p, body(max_tokens=8192))
        assert status == 400
        assert "実行 r1 の上限" in json.loads(data)["error"]["message"]
        assert fake.received == []
        # 別の run はまだ送れる。
        p.run(run_id="r2", cap_usd=1)
        status, _, _ = post(p, body(max_tokens=8192))
        assert status == 200
        assert len(fake.received) == 1


@pytest.mark.parametrize("which", ["run", "eval"])
def test_parallel_requests_never_overbook(which):
    cfg = load_config()
    payload = body(max_tokens=8192)
    raw_len = len(json.dumps(payload, ensure_ascii=False).encode())
    worst = cfg.pricing.reserve_micros(MODEL, raw_len, 8192)
    room = worst * 2 + worst // 2  # 2 本だけ入る
    slow = Reply(
        headers={"content-type": "text/event-stream"}, chunks=sse(*standard_events()), before=0.8
    )
    with FakeUpstream(lambda _r: slow) as f, running_proxy(f.url) as p:
        if which == "run":
            p.run(cap_usd=room / 1_000_000)
        else:
            _prefill(
                cfg.paths.ledger, {"run_id": "x", "cost_micros": cfg.caps.eval_cap_micros - room}
            )
            p.run()
        results: list[int] = []
        lock = threading.Lock()

        def send():
            status, _, _ = post(p, payload)
            with lock:
                results.append(status)

        threads = [threading.Thread(target=send) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert sorted(results) == [200, 200, 400, 400]
        assert len(f.received) == 2
        assert f.max_in_flight <= 2
        spend = p.control("GET", "/abeval/spend").json()
        assert spend["reserved_micros"] == 0
        assert len([e for e in ledger_entries(p) if e["run_id"] == "r1"]) == 2


@pytest.mark.parametrize("status", [429, 403, 500, 529])
def test_error_statuses_pass_through_and_cost_nothing(status):
    payload = json.dumps(
        {"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}}
    ).encode()
    reply = Reply(
        status=status,
        headers={"content-type": "application/json", "retry-after": "7"},
        chunks=[payload],
    )
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        got, hdrs, data = post(p, body())
        assert got == status
        assert data == payload
        assert hdrs["retry-after"] == "7"
        assert len(f.received) == 1
        [entry] = ledger_entries(p)
        assert entry["status"] == status
        assert entry["cost_micros"] == 0
        assert entry["usage_missing"] is False


def test_usage_takes_the_larger_of_start_and_delta():
    events = standard_events(start_out=1, final_out=50, input_tokens=25)
    # 互換の中継が message_delta に input_tokens: 0 を載せても、message_start の値を消さない。
    events[4]["usage"] = {"input_tokens": 0, "output_tokens": 50}
    reply = Reply(headers={"content-type": "text/event-stream"}, chunks=sse(*events))
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        post(p, body())
        [entry] = ledger_entries(p)
        assert entry["usage"]["output_tokens"] == 50
        assert entry["usage"]["input_tokens"] == 25
        # ⌈25 × 0.1 ＋ 50 × 0.5⌉ ＝ ⌈27.5⌉ ＝ 28 マイクロ。
        assert entry["cost_micros"] == 28
        assert entry["stop_reason"] == "end_turn"
        assert entry["resp_model"] == MODEL
        assert entry["usage_missing"] is False
        assert entry["tag"] == "abeval/e0/r1/t1"


def test_non_stream_json_is_read():
    msg = {
        "id": "msg_x",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 1_000, "output_tokens": 200},
    }
    reply = Reply(headers={"content-type": "application/json"}, chunks=[json.dumps(msg).encode()])
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        status, _, data = post(p, body(stream=False))
        assert status == 200
        assert json.loads(data) == msg
        [entry] = ledger_entries(p)
        assert entry["cost_micros"] == 100 + 100
        assert entry["stop_reason"] == "tool_use"


def test_truncated_stream_is_charged_the_reservation():
    events = standard_events()[:3]  # message_delta が来ない
    reply = Reply(headers={"content-type": "text/event-stream"}, chunks=sse(*events))
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        post(p, body())
        [entry] = ledger_entries(p)
        assert entry["usage_missing"] is True
        assert entry["cost_micros"] == entry["reserved_micros"] > 0
        assert len(f.received) == 1


def test_count_tokens_is_not_accounted():
    reply = Reply(headers={"content-type": "application/json"}, chunks=[b'{"input_tokens": 42}'])
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        status, _, data = post(
            p, body(stream=None, max_tokens=None), path="/v1/messages/count_tokens"
        )
        assert status == 200
        assert json.loads(data) == {"input_tokens": 42}
        assert f.received[0].path == "/v1/messages/count_tokens"
        assert ledger_entries(p) == []
        # 許可モデルは照合する。
        status, _, _ = post(
            p, body(model="claude-opus-5-5", max_tokens=None), path="/v1/messages/count_tokens"
        )
        assert status == 400
        assert len(f.received) == 1


def test_capture_only_returns_sse_without_upstream(fake):
    with running_proxy(fake.url) as p:
        p.run(run_id="cap1", capture_only=True)
        payload = body(tools=[{"name": "emit_workflow", "description": "…", "input_schema": {}}])
        status, hdrs, data = post(p, payload)
        assert status == 200
        assert hdrs["content-type"] == "text/event-stream"
        reader = SseReader()
        reader.feed(data)
        meta = reader.finish()
        assert meta.started and meta.complete and meta.stopped
        assert meta.stop_reason == "end_turn"
        assert b"abeval capture" in data
        status, _, data = post(p, body(stream=False))
        assert status == 200
        assert json.loads(data)["content"][0]["text"] == "abeval capture"
        assert fake.received == []
        saved = sorted((p.state.cfg.paths.cache / "requests" / "cap1").glob("*[0-9]-t1.json"))
        assert len(saved) == 2
        assert json.loads(saved[0].read_bytes()) == payload
        entries = ledger_entries(p)
        assert [e["cost_micros"] for e in entries] == [0, 0]
        assert all(e["captured"] for e in entries)


def test_no_client_secret_in_ledger_or_records(fake, tmp_path):
    with running_proxy(fake.url, capture=True) as p:
        p.run(run_id="fwd")
        secret_headers = headers(authorization=f"Bearer {CLIENT_SECRET}")
        post(p, body(), hdrs=secret_headers)
        p.run(run_id="cap", capture_only=True)
        post(p, body(), hdrs=secret_headers)
        assert len(fake.received) == 1
        saved = list((p.state.cfg.paths.cache / "requests").rglob("*.json"))
        assert len(saved) == 4  # 本文と meta が 2 組
    scanned = 0
    for root, _dirs, files in os.walk(tmp_path):
        for name in files:
            path = os.path.join(root, name)
            with open(path, "rb") as f:
                content = f.read()
            scanned += 1
            assert CLIENT_SECRET.encode() not in content, path
    assert scanned >= 6


def test_control_requires_the_token(fake):
    with running_proxy(fake.url) as p:
        token_file = p.state.cfg.paths.home / "proxy.token"
        assert token_file.read_text() == p.state.token
        assert oct(token_file.stat().st_mode & 0o777) == oct(0o600)
        run = {"run_id": "r1", "exp": "e0", "cap_usd": 1}
        for token in (None, "wrong"):
            assert p.control("POST", "/abeval/runs", run, token=token).status_code == 403
            assert p.control("GET", "/abeval/spend", token=token).status_code == 403
            assert (
                p.control("PUT", "/abeval/active", {"run_id": None}, token=token).status_code == 403
            )
            assert p.control("DELETE", "/abeval/runs/r1", token=token).status_code == 403
            assert (
                p.control(
                    "POST",
                    "/abeval/sessions",
                    {"session": "s", "trial": "t", "run_id": "r1"},
                    token,
                ).status_code
                == 403
            )
        assert p.control("POST", "/abeval/runs", run).status_code == 201
        spend = p.control("GET", "/abeval/spend").json()
        assert spend["runs"]["r1"]["cap_micros"] == 1_000_000
        assert spend["eval_cap_micros"] == 35_000_000
        assert p.control("DELETE", "/abeval/runs/r1").status_code == 204
        assert p.control("DELETE", "/abeval/runs/r1").status_code == 404


@pytest.mark.parametrize(
    "bad",
    [
        {"run_id": "../x", "exp": "e0", "cap_usd": 1},
        {"run_id": "r1", "exp": "e0", "cap_usd": 0},
        {"run_id": "r1", "exp": "e0", "cap_usd": True},
        {"run_id": "r1", "exp": "e0", "cap_usd": 1, "capture_only": "yes"},
    ],
)
def test_bad_run_registration(fake, bad):
    with running_proxy(fake.url) as p:
        assert p.control("POST", "/abeval/runs", bad).status_code == 400


def test_run_id_cannot_move_to_another_experiment(fake):
    with running_proxy(fake.url) as p:
        p.run(run_id="r1", exp="e0")
        r = p.control("POST", "/abeval/runs", {"run_id": "r1", "exp": "e1", "cap_usd": 1})
        assert r.status_code == 409


@pytest.mark.parametrize(
    "method, path",
    [
        ("GET", "/v1/messages"),
        ("POST", "/v1/complete"),
        ("POST", "/v1/models"),
        ("GET", "/spend"),
        ("PATCH", "/v1/messages"),
        ("DELETE", "/v1/messages"),
        ("PUT", "/v1/messages"),
        ("GET", "/"),
    ],
)
def test_other_paths_are_forbidden(fake, method, path):
    with running_proxy(fake.url) as p:
        p.run()
        with httpx.Client(trust_env=False, timeout=10) as c:
            r = c.request(method, p.url + path, json=body(), headers=headers())
        assert r.status_code == 403
        assert r.json()["error"]["type"] == "permission_error"
        assert fake.received == []


@pytest.mark.parametrize("host", ["0.0.0.0", "::", "192.168.10.2", "example.com"])
def test_bind_outside_loopback_is_refused(host):
    state = ProxyState(load_config(), upstream="http://127.0.0.1:1")
    with pytest.raises(ValueError):
        ProxyServer(state, host, 0)


def _closed_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_upstream_down_is_502_and_costs_nothing():
    with running_proxy(f"http://127.0.0.1:{_closed_port()}") as p:
        p.run()
        status, _, data = post(p, body())
        assert status == 502
        assert json.loads(data)["error"]["type"] == "api_error"
        assert ledger_entries(p) == []
        spend = p.control("GET", "/abeval/spend").json()
        assert spend["reserved_micros"] == 0
        # 台帳に載らないので、実行ごとに数えて返す（Mode B が基盤の失敗を見分ける）。
        assert spend["runs"]["r1"]["unreachable"] == 1 and spend["runs"]["r1"]["rejected"] == 0


def test_rejections_are_counted_per_run(fake):
    with running_proxy(fake.url) as p:
        p.run(cap_usd=0.000001)
        status, _, _ = post(p, body())
        assert status == 400
        status, _, _ = post(p, body(model="claude-other-1"))
        assert status == 400
        p.control("PUT", "/abeval/active", {"run_id": None})
        post(p, body())  # 割り当たらない
        spend = p.control("GET", "/abeval/spend").json()
        r = spend["runs"]["r1"]
        assert r["rejected"] == 2 and "許可リスト" in r["last_rejection"]
        assert spend["unassigned_rejected"] == 1
        assert fake.received == [] and ledger_entries(p) == []


def test_received_requests_keep_the_body(fake):
    with running_proxy(fake.url) as p:
        p.run()
        payload = body(messages=[{"role": "user", "content": "日報を週ごとにまとめて"}])
        post(p, payload)
        got: Received = fake.received[0]
        assert got.json == payload
        assert got.path == "/v1/messages"


def test_sse_reply_helper_shape():
    r = sse_reply()
    assert r.headers["content-type"] == "text/event-stream"
    assert len(r.chunks) == 6


@pytest.mark.parametrize("value", ["../x", "has space", "sk-ant-api03-z", "x" * 200])
def test_bad_session_header_is_rejected(fake, value):
    with running_proxy(fake.url) as p:
        p.run()
        status, _, data = post(p, body(), hdrs=headers(**{"x-abeval-session": value}))
        assert status == 400
        assert "x-abeval-session" in json.loads(data)["error"]["message"]
        assert fake.received == []


def test_failed_ledger_write_keeps_the_reservation(fake, monkeypatch):
    with running_proxy(fake.url) as p:
        p.run()

        def broken(_entry):
            raise OSError("disk full")

        monkeypatch.setattr(p.state.ledger, "append", broken)
        status, _, _ = post(p, body(max_tokens=8192))
        assert status == 200
        # 使ったかもしれない額を、この中継が動いている間は上限に数え続ける。
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            spend = p.control("GET", "/abeval/spend").json()
            if spend["reserved_micros"] > 0:
                break
            time.sleep(0.05)
        assert spend["reserved_micros"] > 0
        assert spend["runs"]["r1"]["reserved_micros"] == spend["reserved_micros"]


def test_ledger_keeps_only_counts_and_short_words():
    from abeval.proxy import _counts_only, _safe_word

    usage = {
        "input_tokens": 3,
        "service_tier": "standard",
        "cache_creation": {"ephemeral_5m_input_tokens": 2, "note": "x"},
        "bad key": 5,
        "flag": True,
    }
    assert _counts_only(usage) == {
        "input_tokens": 3,
        "cache_creation": {"ephemeral_5m_input_tokens": 2},
    }
    assert _safe_word("end_turn") == "end_turn"
    assert _safe_word("sk-ant-api03 secret") == "<unexpected>"
    assert _safe_word("sk-ant-api03-abc") == "<unexpected>"
    assert _counts_only({"x_api_key_tokens": 1, "output_tokens": 2}) == {"output_tokens": 2}
    assert _safe_word(None) is None


def test_client_disconnect_is_charged_the_reservation():
    reply = Reply(
        headers={"content-type": "text/event-stream"}, chunks=sse(*standard_events()), delay=0.3
    )
    with FakeUpstream(lambda _r: reply) as f, running_proxy(f.url) as p:
        p.run()
        raw = json.dumps(body()).encode()
        with (
            httpx.Client(trust_env=False, timeout=30) as c,
            c.stream("POST", p.url + "/v1/messages", content=raw, headers=headers()) as r,
        ):
            next(r.iter_raw())  # 最初のチャンクだけ読んで切る
        deadline = time.monotonic() + 10
        entries: list[dict] = []
        while time.monotonic() < deadline and not entries:
            entries = ledger_entries(p)
            time.sleep(0.05)
        [entry] = entries
        assert entry["client_gone"] is True
        assert entry["usage_missing"] is True
        assert entry["cost_micros"] == entry["reserved_micros"] > 0
        assert p.control("GET", "/abeval/spend").json()["reserved_micros"] == 0
    # 偽の上流と中継の応答のスレッドは、閉じるときに終わっている（テストの後まで書き続けない）。
    lingering = [t for t in threading.enumerate() if "process_request_thread" in t.name]
    assert lingering == []


def test_stopping_writes_inflight_requests_at_their_reservation():
    """止めるとき（Ctrl-C・SIGTERM）は、処理中の要求を予約の額で書く。後から終わっても二重に書かない。"""
    cfg = load_config()
    state = ProxyState(cfg, upstream="http://127.0.0.1:9")
    state.register_run({"run_id": "r1", "exp": "e0", "cap_usd": 1.0})
    state.set_active({"run_id": "r1", "trial": "t1"})
    raw = json.dumps(body()).encode()
    ticket = state.admit(body(), len(raw), None, False)
    assert ticket.reserved > 0
    assert state.abandon_inflight() == 1
    [entry] = Ledger(cfg.paths.ledger).entries()
    assert entry["cost_micros"] == ticket.reserved and entry["usage_missing"] is True
    assert entry["abandoned"] is True and entry["run_id"] == "r1" and entry["trial"] == "t1"
    assert state.spend()["reserved_micros"] == 0
    state.settle(ticket, {"cost_micros": 1, "run_id": "r1"})
    assert len(Ledger(cfg.paths.ledger).entries()) == 1
    assert state.abandon_inflight() == 0
