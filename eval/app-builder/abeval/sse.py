"""Anthropic の Messages の応答から usage・停止理由・モデルを読む（中継と Mode A が共有する）。

- SSE は流れてきたバイトをそのまま `feed` に渡す（中身は変えない・行の途中で切れていてもよい）。
- usage は `message_start` の `message.usage` と `message_delta` の `usage` を、フィールドごとに大きい方で積む
  （製品の `crates/llm-gateway/src/providers/usage.rs` の `AnthropicUsage::absorb` と同じ。互換の中継が
  `message_delta` に `input_tokens: 0` を載せても `message_start` の値を消さない）。入れ子の
  `cache_creation`（5 分・1 時間の内訳）もフィールドごとに大きい方。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field


def _is_count(v: object) -> bool:
    return isinstance(v, int | float) and not isinstance(v, bool)


def merge_usage(acc: dict, u: object) -> None:
    """usage を 1 つ取り込む（数はフィールドごとに大きい方・数でない値は後のもの）。"""
    if not isinstance(u, dict):
        return
    for k, v in u.items():
        if _is_count(v):
            prev = acc.get(k)
            acc[k] = max(prev, v) if _is_count(prev) else v
        elif isinstance(v, dict):
            sub = acc.get(k)
            if not isinstance(sub, dict):
                sub = {}
                acc[k] = sub
            merge_usage(sub, v)
        elif v is not None:
            acc[k] = v


@dataclass
class MessageMeta:
    """応答から読んだもの。`complete` は最後の usage（`message_delta`）まで届いたか。"""

    usage: dict = field(default_factory=dict)
    stop_reason: str | None = None
    model: str | None = None
    started: bool = False
    complete: bool = False
    stopped: bool = False
    error: dict | None = None


class SseReader:
    """SSE のバイトを少しずつ受けて、イベントの data を読む。"""

    def __init__(self) -> None:
        self.meta = MessageMeta()
        self._buf = b""
        self._data: list[bytes] = []

    def feed(self, chunk: bytes) -> None:
        self._buf += chunk
        while True:
            nl = self._buf.find(b"\n")
            if nl < 0:
                return
            line, self._buf = self._buf[:nl], self._buf[nl + 1 :]
            self._line(line.rstrip(b"\r"))

    def finish(self) -> MessageMeta:
        """流れが終わった（残りを読み切る）。"""
        if self._buf:
            self._line(self._buf.rstrip(b"\r"))
            self._buf = b""
        self._dispatch()
        return self.meta

    def _line(self, line: bytes) -> None:
        if not line:
            self._dispatch()
        elif line.startswith(b"data:"):
            data = line[5:]
            self._data.append(data[1:] if data.startswith(b" ") else data)

    def _dispatch(self) -> None:
        if not self._data:
            return
        raw = b"\n".join(self._data)
        self._data = []
        try:
            ev = json.loads(raw)
        except ValueError:
            return
        if isinstance(ev, dict):
            self._event(ev)

    def _event(self, ev: dict) -> None:
        m = self.meta
        kind = ev.get("type")
        if kind == "message_start":
            msg = ev.get("message") or {}
            if isinstance(msg, dict):
                m.started = True
                if isinstance(msg.get("model"), str):
                    m.model = msg["model"]
                merge_usage(m.usage, msg.get("usage"))
        elif kind == "message_delta":
            merge_usage(m.usage, ev.get("usage"))
            delta = ev.get("delta") or {}
            if isinstance(delta, dict) and isinstance(delta.get("stop_reason"), str):
                m.stop_reason = delta["stop_reason"]
            m.complete = True
        elif kind == "message_stop":
            m.stopped = True
        elif kind == "error":
            err = ev.get("error")
            m.error = err if isinstance(err, dict) else {"message": str(err)}


def read_message(body: bytes) -> MessageMeta:
    """非ストリームの応答（JSON の message）から読む。読めなければ空（complete=False）。"""
    meta = MessageMeta()
    try:
        msg = json.loads(body)
    except ValueError:
        return meta
    if not isinstance(msg, dict):
        return meta
    if msg.get("type") == "error":
        err = msg.get("error")
        meta.error = err if isinstance(err, dict) else {"message": str(err)}
        return meta
    if isinstance(msg.get("model"), str):
        meta.model = msg["model"]
    if isinstance(msg.get("stop_reason"), str):
        meta.stop_reason = msg["stop_reason"]
    if isinstance(msg.get("usage"), dict):
        merge_usage(meta.usage, msg["usage"])
        meta.started = meta.complete = meta.stopped = True
    return meta
