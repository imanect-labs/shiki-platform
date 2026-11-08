"""偽の Anthropic の応答（Messages の SSE の決まった流れ・架空の中身）。

`events_*` はイベント（dict）の列を返す。`fake_upstream.sse(*events)` でチャンクにするか、`sse_bytes` で 1 つの
本文にする（httpx.MockTransport 用）。思考の文と応答の文には印（`THINKING_MARK`・`TEXT_MARK`）を入れておき、
結果やレポートに会話の全文が漏れていないことを確かめるのに使う。
"""

from __future__ import annotations

import json

MODEL = "claude-haiku-5-5"
THINKING_MARK = "思考の印-7f3a"
TEXT_MARK = "応答の印-c19e"


def _start(input_tokens: int = 1200, out: int = 1) -> dict:
    return {
        "type": "message_start",
        "message": {
            "id": "msg_fake",
            "type": "message",
            "role": "assistant",
            "model": MODEL,
            "content": [],
            "stop_reason": None,
            "usage": {"input_tokens": input_tokens, "output_tokens": out},
        },
    }


def _thinking(index: int) -> list[dict]:
    return [
        {
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "thinking", "thinking": ""},
        },
        {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "thinking_delta", "thinking": THINKING_MARK},
        },
        {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "signature_delta", "signature": "sig"},
        },
        {"type": "content_block_stop", "index": index},
    ]


def _text(index: int, text: str) -> list[dict]:
    half = len(text) // 2
    return [
        {
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "text", "text": ""},
        },
        {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "text_delta", "text": text[:half]},
        },
        {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "text_delta", "text": text[half:]},
        },
        {"type": "content_block_stop", "index": index},
    ]


def _tool_use(index: int, name: str, payload: dict, *, close: bool = True) -> list[dict]:
    raw = json.dumps(payload, ensure_ascii=False)
    step = max(len(raw) // 3, 1)
    parts = [raw[i : i + step] for i in range(0, len(raw), step)]
    evs = [
        {
            "type": "content_block_start",
            "index": index,
            "content_block": {
                "type": "tool_use",
                "id": f"toolu_{index}",
                "name": name,
                "input": {},
            },
        }
    ]
    evs += [
        {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": p},
        }
        for p in parts
    ]
    if close:
        evs.append({"type": "content_block_stop", "index": index})
    return evs


def _end(stop: str, out: int, details: dict | None = None) -> list[dict]:
    delta: dict = {"stop_reason": stop, "stop_sequence": None}
    if details is not None:
        delta["stop_details"] = details
    return [
        {"type": "message_delta", "delta": delta, "usage": {"output_tokens": out}},
        {"type": "message_stop"},
    ]


def events_emit(ir: dict | str, *, input_tokens: int = 1200, out: int = 800) -> list[dict]:
    """思考 → 短い文 → emit_workflow の呼び出し（停止理由 tool_use）。"""
    return [
        _start(input_tokens),
        *_thinking(0),
        *_text(1, f"フローを作ります。{TEXT_MARK}"),
        *_tool_use(2, "emit_workflow", {"ir": ir}),
        *_end("tool_use", out),
    ]


def events_text(text: str = f"できました。{TEXT_MARK}", *, out: int = 50) -> list[dict]:
    return [_start(), *_thinking(0), *_text(1, text), *_end("end_turn", out)]


def events_ask(*, out: int = 40) -> list[dict]:
    return events_text(f"{TEXT_MARK} 保存先のフォルダーはどこにしますか？", out=out)


def events_max_tokens(ir: dict) -> list[dict]:
    """emit_workflow の入力の途中で max_tokens に達した。"""
    return [
        _start(),
        *_thinking(0),
        *_tool_use(1, "emit_workflow", {"ir": ir}, close=False)[:-1],
        *_end("max_tokens", 8192),
    ]


def events_refusal() -> list[dict]:
    return [
        _start(),
        *_end("refusal", 0, {"type": "refusal", "category": "general_harms", "explanation": None}),
    ]


def events_overloaded() -> list[dict]:
    """流れの途中の error（overloaded）。"""
    return [
        _start(),
        {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}},
    ]


def sse_bytes(events: list[dict]) -> bytes:
    return b"".join(
        f"event: {e['type']}\ndata: {json.dumps(e, ensure_ascii=False)}\n\n".encode()
        for e in events
    )
