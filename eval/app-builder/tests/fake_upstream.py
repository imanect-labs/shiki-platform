"""偽の上流（ループバックの一時のポートで動く stdlib のサーバー・受けた要求を記録する）。

応答は HTTP/1.0 の close 区切り（手元の中継と同じ）。`responder` で応答の中身を決める。

応答のスレッドは daemon にせず、閉じるときに終わりを待つ（`block_on_close`）。daemon のままだと、相手に切られた
応答のスレッドがテストの後も書き続け、インタプリタの終了と重なって Python が異常終了する（exit 134）。切られた
接続への書き込み（BrokenPipeError・ConnectionResetError）は握り、標準エラーにトレースバックを出さない。
"""

from __future__ import annotations

import json
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@dataclass
class Received:
    path: str
    headers: dict[str, str]
    body: bytes

    @property
    def json(self) -> dict:
        return json.loads(self.body)


@dataclass
class Reply:
    """偽の上流の応答。`chunks` は 1 つずつ書いて flush する（間に `delay` 秒置く）。"""

    status: int = 200
    headers: dict[str, str] = field(default_factory=dict)
    chunks: list[bytes] = field(default_factory=list)
    delay: float = 0.0
    before: float = 0.0


def sse(*events: dict) -> list[bytes]:
    """SSE のイベントを 1 チャンクずつにする。"""
    return [f"event: {e['type']}\ndata: {json.dumps(e)}\n\n".encode() for e in events]


def standard_events(
    model: str = "claude-haiku-5-5", start_out: int = 1, final_out: int = 50, input_tokens: int = 25
) -> list[dict]:
    return [
        {
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": None,
                "usage": {"input_tokens": input_tokens, "output_tokens": start_out},
            },
        },
        {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""},
        },
        {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "こんにちは"},
        },
        {"type": "content_block_stop", "index": 0},
        {
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": None},
            "usage": {"output_tokens": final_out},
        },
        {"type": "message_stop"},
    ]


def sse_reply(**kw) -> Reply:
    return Reply(
        headers={"content-type": "text/event-stream", "request-id": "req_fake"},
        chunks=sse(*standard_events(**kw)),
    )


class QuietServer(ThreadingHTTPServer):
    """応答のスレッドの終わりを待って閉じる・誤りのトレースバックを出さないサーバー。"""

    daemon_threads = False
    block_on_close = True

    def handle_error(self, request, client_address):  # 切られた接続などを標準エラーに出さない
        pass


class FakeUpstream:
    def __init__(self, responder: Callable[[Received], Reply] | None = None):
        self.responder = responder or (lambda _req: sse_reply())
        self.received: list[Received] = []
        self._lock = threading.Lock()
        self.in_flight = 0
        self.max_in_flight = 0
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, format, *args):
                pass

            def do_POST(self):
                n = int(self.headers.get("content-length", "0"))
                body = self.rfile.read(n)
                req = Received(
                    path=self.path,
                    headers={k.lower(): v for k, v in self.headers.items()},
                    body=body,
                )
                with outer._lock:
                    outer.received.append(req)
                    outer.in_flight += 1
                    outer.max_in_flight = max(outer.max_in_flight, outer.in_flight)
                try:
                    reply = outer.responder(req)
                    if reply.before:
                        time.sleep(reply.before)
                    self.send_response_only(reply.status)
                    for k, v in reply.headers.items():
                        self.send_header(k, v)
                    self.end_headers()
                    for chunk in reply.chunks:
                        self.wfile.write(chunk)
                        self.wfile.flush()
                        if reply.delay:
                            time.sleep(reply.delay)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # 相手（中継）が途中で切った
                finally:
                    with outer._lock:
                        outer.in_flight -= 1

        self._server = QuietServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(
            target=self._server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
        )

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def __enter__(self) -> FakeUpstream:
        self._thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self._server.shutdown()
        self._server.server_close()  # 応答のスレッドの終わりを待つ
        self._thread.join(timeout=10)
