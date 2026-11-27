"""Anthropic の Messages API を httpx で直接叩く（Mode A の生成・GAP-30 §4.6 の 2）。

SDK は使わない（自動の再試行と勝手に付くヘッダーを避け、要求の形を自分で決めるため）。送り先は abeval.proxy
（`127.0.0.1:18200`）で、鍵は扱わない（`x-api-key` はダミー。本物の鍵は手元の中継が付ける）。

- 要求は stream で送る。`temperature`・`top_p`・`top_k` と assistant の prefill は送る前に拒む（Haiku 5.5 は
  400 を返す・製品のアダプタも送らない）。
- 再試行は 429・5xx（529 を含む）・接続の誤り・流れの途中の `error` イベント（overloaded・api_error）だけ。
  400・403 など、ほかの 4xx は再試行しない（abeval.proxy の予算の拒否は 400、手元の中継の拒否は 403）。
  待ち時間は指数で伸ばし、全幅のジッタを付ける。`retry-after` があればそれより短くしない。回数と待ちは
  `attempts` に残す。
- SSE からブロック（text・thinking・tool_use・ほか）を組み立てる。tool_use の入力は `input_json_delta` を
  つないで最後に JSON として読む（読めなければ `input` は None で `input_error` に理由）。usage は
  `abeval.sse.merge_usage`（`message_start` と `message_delta` のフィールドごとの大きい方）。
"""

from __future__ import annotations

import json
import random
import time
from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any

import httpx

from abeval.shiki import iter_sse
from abeval.sse import merge_usage

DUMMY_CREDENTIAL = "abeval-dummy"
ANTHROPIC_VERSION = "2023-06-01"
SAMPLING_PARAMS = ("temperature", "top_p", "top_k")
RETRY_STREAM_ERRORS = frozenset({"overloaded_error", "api_error", "rate_limit_error"})
DEFAULT_MAX_ATTEMPTS = 4
BACKOFF_BASE = 1.0
BACKOFF_CAP = 30.0


class RequestShapeError(ValueError):
    """送ってはいけない形の要求（送る前に止める）。"""


@dataclass
class Attempt:
    """1 回の送信。`status` は HTTP の状態（繋がらなければ None）、`error` は誤りの種類か例外の型名。"""

    status: int | None
    error: str | None = None
    wait: float = 0.0
    retried: bool = False

    def to_dict(self) -> dict:
        return {"status": self.status, "error": self.error, "wait": round(self.wait, 3)}


@dataclass
class MessageResult:
    """組み立てた応答（成功・失敗のどちらも）。`ok` は 200 で最後まで届いたもの。"""

    status: int | None
    content: list[dict] = field(default_factory=list)
    stop_reason: str | None = None
    stop_details: Any = None
    usage: dict = field(default_factory=dict)
    model: str | None = None
    error: dict | None = None
    request_id: str | None = None
    complete: bool = False
    attempts: list[Attempt] = field(default_factory=list)
    secs: float = 0.0

    @property
    def ok(self) -> bool:
        return self.status == 200 and self.complete and self.error is None

    @property
    def retries(self) -> int:
        return max(len(self.attempts) - 1, 0)

    def tool_uses(self, name: str | None = None) -> list[dict]:
        return [
            b
            for b in self.content
            if b.get("type") == "tool_use" and (name is None or b.get("name") == name)
        ]

    def text(self) -> str:
        return "".join(b.get("text", "") for b in self.content if b.get("type") == "text")


def check_request(body: Mapping) -> None:
    """送ってはいけない形を止める（サンプリングの項目・prefill・stream でない）。"""
    bad = [p for p in SAMPLING_PARAMS if p in body]
    if bad:
        raise RequestShapeError(f"{', '.join(bad)} は送りません（Haiku 5.5 は 400）")
    msgs = body.get("messages")
    if not isinstance(msgs, list) or not msgs:
        raise RequestShapeError("messages がありません")
    if isinstance(msgs[-1], Mapping) and msgs[-1].get("role") == "assistant":
        raise RequestShapeError("assistant の prefill は送りません（Haiku 5.5 は 400）")
    if body.get("stream") is not True:
        raise RequestShapeError("stream: true で送ります")
    mt = body.get("max_tokens")
    if isinstance(mt, bool) or not isinstance(mt, int) or mt <= 0:
        raise RequestShapeError("max_tokens（正の整数）がありません")


class _Assembler:
    """SSE のイベントからブロックと usage を組み立てる。"""

    def __init__(self, result: MessageResult):
        self.r = result
        self.blocks: dict[int, dict] = {}
        self.json_parts: dict[int, list[str]] = {}

    def event(self, ev: Mapping) -> None:
        kind = ev.get("type")
        r = self.r
        if kind == "message_start":
            msg = ev.get("message") or {}
            if isinstance(msg, Mapping):
                if isinstance(msg.get("model"), str):
                    r.model = msg["model"]
                merge_usage(r.usage, msg.get("usage"))
        elif kind == "content_block_start":
            i = ev.get("index")
            cb = ev.get("content_block")
            if isinstance(i, int) and isinstance(cb, Mapping):
                block = dict(cb)
                if block.get("type") == "tool_use":
                    self.json_parts[i] = []
                    block["input"] = block.get("input") or {}
                self.blocks[i] = block
        elif kind == "content_block_delta":
            self._delta(ev.get("index"), ev.get("delta") or {})
        elif kind == "content_block_stop":
            self._stop(ev.get("index"))
        elif kind == "message_delta":
            merge_usage(r.usage, ev.get("usage"))
            delta = ev.get("delta") or {}
            if isinstance(delta, Mapping):
                if isinstance(delta.get("stop_reason"), str):
                    r.stop_reason = delta["stop_reason"]
                if delta.get("stop_details") is not None:
                    r.stop_details = delta["stop_details"]
            r.complete = True
        elif kind == "error":
            err = ev.get("error")
            r.error = dict(err) if isinstance(err, Mapping) else {"message": str(err)}

    def _delta(self, i: object, delta: Mapping) -> None:
        block = self.blocks.get(i) if isinstance(i, int) else None
        if block is None or not isinstance(delta, Mapping):
            return
        kind = delta.get("type")
        if kind == "text_delta":
            block["text"] = block.get("text", "") + str(delta.get("text", ""))
        elif kind == "input_json_delta":
            self.json_parts.setdefault(i, []).append(str(delta.get("partial_json", "")))
        elif kind == "thinking_delta":
            block["thinking"] = block.get("thinking", "") + str(delta.get("thinking", ""))
        elif kind == "signature_delta":
            block["signature"] = block.get("signature", "") + str(delta.get("signature", ""))

    def _stop(self, i: object) -> None:
        block = self.blocks.get(i) if isinstance(i, int) else None
        if block is None or block.get("type") != "tool_use":
            return
        raw = "".join(self.json_parts.pop(i, []))
        if not raw.strip():
            return  # 入力が content_block_start にだけある（空の入力）
        try:
            block["input"] = json.loads(raw)
        except ValueError as e:
            block["input"] = None
            block["input_error"] = f"tool_use の入力が JSON として読めません: {e}"

    def finish(self) -> None:
        # 閉じられなかった tool_use（途中で切れた）は入力を読めないものとして残す。
        for i in list(self.json_parts):
            block = self.blocks.get(i)
            if block is not None:
                block["input"] = None
                block["input_error"] = "tool_use が閉じられていません（途中で切れた）"
        self.json_parts.clear()
        self.r.content = [self.blocks[i] for i in sorted(self.blocks)]


def _retry_after(headers: Mapping[str, str]) -> float | None:
    v = headers.get("retry-after")
    if v is None:
        return None
    try:
        return max(float(v), 0.0)
    except ValueError:
        return None


def _error_body(resp: httpx.Response) -> dict:
    try:
        body = resp.json()
    except ValueError:
        return {"type": "http_error", "message": resp.text[:300]}
    err = body.get("error") if isinstance(body, Mapping) else None
    return (
        dict(err)
        if isinstance(err, Mapping)
        else {"type": "http_error", "message": str(body)[:300]}
    )


def retryable_status(status: int) -> bool:
    return status == 429 or 500 <= status < 600


class AnthropicClient:
    """abeval.proxy（か互換の口）へ Messages の要求を送る。"""

    def __init__(
        self,
        base_url: str,
        *,
        session_header: str = "x-abeval-session",
        transport: httpx.BaseTransport | None = None,
        timeout: float = 600.0,
        max_attempts: int = DEFAULT_MAX_ATTEMPTS,
        sleep: Callable[[float], None] = time.sleep,
        rng: random.Random | None = None,
        clock: Callable[[], float] = time.monotonic,
    ):
        if max_attempts < 1:
            raise ValueError("max_attempts は 1 以上です")
        self.base_url = base_url.rstrip("/")
        self.session_header = session_header
        self.max_attempts = max_attempts
        self._sleep = sleep
        self._rng = rng or random.Random()
        self._clock = clock
        self._http = httpx.Client(
            transport=transport,
            timeout=httpx.Timeout(connect=10.0, read=timeout, write=60.0, pool=10.0),
            trust_env=False,
        )

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> AnthropicClient:
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def headers(self, session: str | None) -> dict[str, str]:
        h = {
            "x-api-key": DUMMY_CREDENTIAL,
            "anthropic-version": ANTHROPIC_VERSION,
            "content-type": "application/json",
            "accept": "text/event-stream",
        }
        if session:
            h[self.session_header] = session
        return h

    def backoff(self, attempt: int, retry_after: float | None) -> float:
        """attempt 回目（0 始まり）の失敗の後に待つ秒（全幅のジッタ・retry-after より短くしない）。"""
        cap = min(BACKOFF_CAP, BACKOFF_BASE * (2**attempt))
        wait = self._rng.uniform(0, cap)
        if retry_after is not None:
            wait = max(wait, min(retry_after, BACKOFF_CAP * 2))
        return wait

    def create(self, body: Mapping, *, session: str | None = None) -> MessageResult:
        """送って応答を組み立てる（再試行を含む）。最後の結果を返す（失敗でも例外にしない）。"""
        check_request(body)
        raw = json.dumps(body, ensure_ascii=False).encode()
        attempts: list[Attempt] = []
        started = self._clock()
        result = MessageResult(status=None)
        for i in range(self.max_attempts):
            result, retry_after = self._once(raw, session)
            attempts.append(Attempt(result.status, _kind(result)))
            if not _should_retry(result) or i == self.max_attempts - 1:
                break
            wait = self.backoff(i, retry_after)
            attempts[-1].wait = wait
            attempts[-1].retried = True
            self._sleep(wait)
        result.attempts = attempts
        result.secs = self._clock() - started
        return result

    def _once(self, raw: bytes, session: str | None) -> tuple[MessageResult, float | None]:
        result = MessageResult(status=None)
        try:
            with self._http.stream(
                "POST", f"{self.base_url}/v1/messages", content=raw, headers=self.headers(session)
            ) as resp:
                result.status = resp.status_code
                result.request_id = resp.headers.get("request-id")
                retry_after = _retry_after(resp.headers)
                if resp.status_code != 200:
                    resp.read()
                    result.error = _error_body(resp)
                    return result, retry_after
                if "text/event-stream" not in resp.headers.get("content-type", ""):
                    resp.read()
                    result.error = {"type": "unexpected_body", "message": resp.text[:300]}
                    return result, None
                asm = _Assembler(result)
                for ev in _events(resp.iter_lines()):
                    asm.event(ev)
                asm.finish()
                return result, None
        except httpx.TransportError as e:
            result.error = {"type": "transport_error", "message": type(e).__name__}
            return result, None


def _events(lines: Iterator[str]) -> Iterator[dict]:
    for ev in iter_sse(lines):
        data = ev.get("data")
        if isinstance(data, dict):
            yield data


def _kind(r: MessageResult) -> str | None:
    if r.error:
        return str(r.error.get("type") or "error")
    if r.status == 200 and not r.complete:
        return "incomplete"
    return None


def _should_retry(r: MessageResult) -> bool:
    if r.status is None:
        return True  # 繋がらない・送った後に切れた（接続の誤り）
    if r.status != 200:
        return retryable_status(r.status)
    if r.error is not None:
        kind = str(r.error.get("type"))
        return kind in RETRY_STREAM_ERRORS or kind == "transport_error"
    return not r.complete  # 200 のまま最後の usage まで届かずに閉じた（接続の誤り）
