from __future__ import annotations

import json

from abeval.sse import SseReader, merge_usage, read_message


def _ev(obj: dict, crlf: bool = False) -> bytes:
    nl = "\r\n" if crlf else "\n"
    return f"event: {obj['type']}{nl}data: {json.dumps(obj)}{nl}{nl}".encode()


START = {
    "type": "message_start",
    "message": {"model": "claude-haiku-5-5", "usage": {"input_tokens": 120, "output_tokens": 1}},
}
DELTA = {
    "type": "message_delta",
    "delta": {"stop_reason": "tool_use"},
    "usage": {"input_tokens": 0, "output_tokens": 77},
}


def test_reads_usage_across_arbitrary_chunk_boundaries():
    data = _ev(START) + _ev({"type": "ping"}) + _ev(DELTA) + _ev({"type": "message_stop"})
    for size in (1, 3, 7, 64, len(data)):
        r = SseReader()
        for i in range(0, len(data), size):
            r.feed(data[i : i + size])
        m = r.finish()
        assert m.usage == {"input_tokens": 120, "output_tokens": 77}, size
        assert (m.model, m.stop_reason) == ("claude-haiku-5-5", "tool_use")
        assert m.started and m.complete and m.stopped


def test_crlf_and_missing_trailing_blank_line():
    r = SseReader()
    r.feed(_ev(START, crlf=True) + f"data: {json.dumps(DELTA)}".encode())
    m = r.finish()
    assert m.complete and m.usage["output_tokens"] == 77


def test_multi_line_data_is_joined():
    payload = json.dumps(DELTA, indent=1).splitlines()
    raw = "".join(f"data: {line}\n" for line in payload) + "\n"
    r = SseReader()
    r.feed(raw.encode())
    assert r.finish().usage["output_tokens"] == 77


def test_error_event_and_no_delta_is_incomplete():
    r = SseReader()
    r.feed(_ev(START) + _ev({"type": "error", "error": {"type": "overloaded_error"}}))
    m = r.finish()
    assert not m.complete
    assert m.error == {"type": "overloaded_error"}


def test_merge_usage_is_fieldwise_max_including_nested():
    acc: dict = {}
    merge_usage(acc, {"input_tokens": 10, "cache_creation": {"ephemeral_5m_input_tokens": 4}})
    merge_usage(
        acc,
        {"input_tokens": 0, "output_tokens": 3, "cache_creation": {"ephemeral_5m_input_tokens": 2}},
    )
    merge_usage(acc, {"service_tier": "standard", "output_tokens": None, "flag": True})
    assert acc == {
        "input_tokens": 10,
        "output_tokens": 3,
        "cache_creation": {"ephemeral_5m_input_tokens": 4},
        "service_tier": "standard",
        "flag": True,
    }


def test_read_message_json_and_errors():
    m = read_message(
        json.dumps(
            {
                "type": "message",
                "model": "m",
                "stop_reason": "end_turn",
                "usage": {"output_tokens": 5},
            }
        ).encode()
    )
    assert m.complete and m.usage == {"output_tokens": 5} and m.stop_reason == "end_turn"
    e = read_message(b'{"type":"error","error":{"type":"rate_limit_error"}}')
    assert not e.complete and e.error == {"type": "rate_limit_error"}
    assert not read_message(b"not json").complete
    assert not read_message(b"[1]").complete
