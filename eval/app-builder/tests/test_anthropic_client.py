"""abeval.anthropic_client（httpx.MockTransport・ソケットを使わない・実 LLM を呼ばない）。"""

from __future__ import annotations

import json
import random

import httpx
import pytest

from abeval.anthropic_client import (
    DUMMY_CREDENTIAL,
    AnthropicClient,
    RequestShapeError,
    check_request,
)
from fake_anthropic import (
    THINKING_MARK,
    events_ask,
    events_emit,
    events_max_tokens,
    events_overloaded,
    events_refusal,
    sse_bytes,
)

IR = {"ir_version": 1, "name": "a", "nodes": [{"id": "n", "type": "script.run"}], "edges": []}


def body(**over) -> dict:
    b = {
        "model": "claude-haiku-5-5",
        "max_tokens": 1024,
        "stream": True,
        "messages": [{"role": "user", "content": "経費の申請を受け付けるフローを作って"}],
    }
    b.update(over)
    return b


def sse_response(events, status=200, headers=None) -> httpx.Response:
    h = {"content-type": "text/event-stream", **(headers or {})}
    return httpx.Response(status, headers=h, content=sse_bytes(events))


def err_response(status: int, etype: str, headers=None) -> httpx.Response:
    return httpx.Response(
        status,
        headers=headers or {},
        json={"type": "error", "error": {"type": etype, "message": f"{etype} です"}},
    )


class Script:
    """応答を順に返す MockTransport（受けた要求を残す）。"""

    def __init__(self, *replies):
        self.replies = list(replies)
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        r = self.replies.pop(0)
        if isinstance(r, Exception):
            raise r
        return r


def client(script: Script, sleeps: list | None = None, **kw) -> AnthropicClient:
    return AnthropicClient(
        "http://127.0.0.1:18200",
        transport=httpx.MockTransport(script),
        sleep=(sleeps.append if sleeps is not None else (lambda s: None)),
        rng=random.Random(7),
        **kw,
    )


@pytest.mark.parametrize("param", ["temperature", "top_p", "top_k"])
def test_sampling_params_are_refused_before_sending(param):
    s = Script()
    with pytest.raises(RequestShapeError):
        client(s).create(body(**{param: 0.5}))
    assert s.seen == []


def test_prefill_and_non_stream_are_refused():
    with pytest.raises(RequestShapeError):
        check_request(
            body(messages=[{"role": "user", "content": "x"}, {"role": "assistant", "content": "{"}])
        )
    with pytest.raises(RequestShapeError):
        check_request(body(stream=False))
    with pytest.raises(RequestShapeError):
        check_request(body(max_tokens=0))


def test_headers_carry_only_the_dummy_key_and_the_session():
    s = Script(sse_response(events_emit(IR)))
    client(s).create(body(), session="t1.flow.0.medium")
    h = s.seen[0].headers
    assert h["x-api-key"] == DUMMY_CREDENTIAL
    assert h["anthropic-version"] == "2023-06-01"
    assert h["x-abeval-session"] == "t1.flow.0.medium"
    assert "authorization" not in h
    sent = json.loads(s.seen[0].content)
    assert "temperature" not in sent and sent["stream"] is True


def test_assembles_thinking_text_and_tool_use_from_partial_json():
    s = Script(sse_response(events_emit(IR, input_tokens=1500, out=900)))
    r = client(s).create(body())
    assert r.ok and r.status == 200 and r.stop_reason == "tool_use"
    kinds = [b["type"] for b in r.content]
    assert kinds == ["thinking", "text", "tool_use"]
    assert r.content[0]["thinking"] == THINKING_MARK
    assert r.tool_uses("emit_workflow")[0]["input"] == {"ir": IR}
    # message_start と message_delta のフィールドごとの大きい方。
    assert r.usage == {"input_tokens": 1500, "output_tokens": 900}
    assert r.model == "claude-haiku-5-5"
    assert len(r.attempts) == 1 and r.retries == 0


def test_truncated_tool_use_has_no_input():
    r = client(Script(sse_response(events_max_tokens(IR)))).create(body())
    assert r.stop_reason == "max_tokens"
    tu = r.tool_uses("emit_workflow")[0]
    assert tu["input"] is None and "閉じられていません" in tu["input_error"]


def test_refusal_and_question_are_not_retried():
    s = Script(sse_response(events_refusal()), sse_response(events_ask()))
    c = client(s)
    r = c.create(body())
    assert r.stop_reason == "refusal" and r.stop_details["category"] == "general_harms"
    assert len(s.seen) == 1
    r2 = c.create(body())
    assert r2.text().endswith("どこにしますか？") and r2.ok


@pytest.mark.parametrize("status", [429, 500, 502, 503, 529])
def test_retries_rate_limits_and_server_errors(status):
    sleeps: list[float] = []
    s = Script(err_response(status, "overloaded_error"), sse_response(events_emit(IR)))
    r = client(s, sleeps).create(body())
    assert r.ok and len(s.seen) == 2
    assert [a.status for a in r.attempts] == [status, 200]
    assert len(sleeps) == 1 and 0 <= sleeps[0] <= 1.0
    assert r.attempts[0].retried and r.attempts[0].wait == sleeps[0]


@pytest.mark.parametrize(
    "status,etype",
    [
        (400, "invalid_request_error"),
        (403, "permission_error"),
        (404, "not_found_error"),
        (413, "request_too_large"),
    ],
)
def test_does_not_retry_client_errors(status, etype):
    sleeps: list[float] = []
    s = Script(err_response(status, etype))
    r = client(s, sleeps).create(body())
    assert not r.ok and r.status == status and r.error["type"] == etype
    assert len(s.seen) == 1 and sleeps == []


def test_retry_after_is_honoured():
    sleeps: list[float] = []
    s = Script(
        err_response(429, "rate_limit_error", {"retry-after": "7"}), sse_response(events_emit(IR))
    )
    client(s, sleeps).create(body())
    assert sleeps[0] >= 7.0


def test_connection_errors_and_mid_stream_overload_are_retried():
    sleeps: list[float] = []
    s = Script(
        httpx.ConnectError("refused"),
        sse_response(events_overloaded()),
        sse_response(events_emit(IR)),
    )
    r = client(s, sleeps).create(body())
    assert r.ok
    assert [a.error for a in r.attempts] == ["transport_error", "overloaded_error", None]
    assert len(sleeps) == 2


def test_gives_up_after_max_attempts_and_backoff_grows():
    sleeps: list[float] = []
    s = Script(*[err_response(529, "overloaded_error") for _ in range(4)])
    r = client(s, sleeps, max_attempts=4).create(body())
    assert not r.ok and r.status == 529 and len(r.attempts) == 4 and len(s.seen) == 4
    assert len(sleeps) == 3
    # 全幅のジッタ: 上限は 1・2・4 秒。
    assert all(0 <= w <= cap for w, cap in zip(sleeps, (1, 2, 4), strict=True))


def test_jitter_is_seeded():
    a = AnthropicClient("http://127.0.0.1:1", rng=random.Random(3))
    b = AnthropicClient("http://127.0.0.1:1", rng=random.Random(3))
    assert [a.backoff(i, None) for i in range(5)] == [b.backoff(i, None) for i in range(5)]
    assert all(a.backoff(10, None) <= 30.0 for _ in range(20))


def test_incomplete_stream_is_retried():
    events = events_emit(IR)[:-2]  # message_delta と message_stop が来ないまま閉じた
    s = Script(sse_response(events), sse_response(events_emit(IR)))
    r = client(s).create(body())
    assert r.ok and [a.error for a in r.attempts] == ["incomplete", None]
