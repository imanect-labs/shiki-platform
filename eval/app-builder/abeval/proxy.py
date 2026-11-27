"""記録・上限つきの中継（Anthropic Messages・鍵は持たない・GAP-30 §4.3）。

製品（評価の版のサーバー）と Mode A は Anthropic への要求をここ（既定 127.0.0.1:18200）に送る。中継は上流
（既定は手元の中継 127.0.0.1:8787。本物の鍵はそこで付く）へ転送し、評価の台帳に記録して上限で止める。

- 受け付ける口: `POST /v1/messages`・`POST /v1/messages/count_tokens`（会計しない）と制御の口 `/abeval/*`
  （ヘッダー `x-abeval-control` に proxy.token の値が要る）。それ以外は 403。
- 送る前に拒む（上流を呼ばず、Anthropic 形式の 400 `invalid_request_error`）: 許可リストに無いモデル・
  temperature／top_p／top_k を含む本文・登録した実行（run）に割り当たらない要求・評価の累計の上限・実行の上限。
  上限の判定は「台帳の累計＋処理中の予約＋この要求の最悪」（最悪は pricing.reserve_micros）。
- 上流へ送るヘッダーは anthropic-version・anthropic-beta・content-type・accept だけ。x-api-key はダミーに
  置き換え、x-campaign-tag（abeval/<exp>/<run>/<trial>）を付ける。クライアントの鍵・会話 ID のヘッダーは送らない。
- 応答は状態コードを含めてそのまま返す。SSE はチャンクごとに中身を変えずに流す（バッファしない）。応答は
  HTTP/1.0 の close 区切り（手元の中継と同じ）。
- 終わったら実際の費用で台帳に 1 行を書いて予約を戻す。usage が最後まで取れなければ予約の額で数え、
  usage_missing の印を付ける。誤りの応答（4xx・5xx を読み切ったもの）は 0 で数える。
- 送る前に拒んだ要求と、上流に繋がらなかった要求は台帳に載らないので、実行（run）ごとに数えて
  `GET /abeval/spend` の `runs[].rejected`・`unreachable`・`last_rejection` で返す（Mode B がチャットの中の
  基盤の失敗を見分けるため。割り当たらない拒否は `unassigned_rejected`）。
- Ctrl-C・SIGTERM で止めるときは、処理中の要求（予約のあるもの）を予約の額で台帳に書いてから終わる
  （`usage_missing`・`client_gone`・`abandoned` の印・上流では課金されたかもしれないので少なく数えない）。
- capture_only の実行は、要求の本文を `<cache>/requests/<run>/` に保存し、上流へ送らずに最小の SSE を返す（$0）。
  `--capture` を付けると、転送した要求の本文も同じ場所に保存する（鍵はヘッダーにあり、本文には入らない）。
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import hmac
import json
import os
import re
import secrets
import signal
import socket
import sys
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

import httpx

from abeval.config import Config, display_path, is_loopback_host, load_config
from abeval.ledger import Ledger
from abeval.pricing import micros_to_usd, usd_to_micros
from abeval.sse import MessageMeta, SseReader, read_message

DUMMY_CREDENTIAL = "abeval-dummy"
CONTROL_HEADER = "x-abeval-control"
TOKEN_FILE = "proxy.token"
PASS_HEADERS = ("anthropic-version", "anthropic-beta", "content-type", "accept")
SAMPLING_PARAMS = ("temperature", "top_p", "top_k")
HOP_HEADERS = {"connection", "keep-alive", "transfer-encoding", "content-length"}
MESSAGES = "/v1/messages"
COUNT_TOKENS = "/v1/messages/count_tokens"
MAX_BODY_BYTES = 32 * 1024 * 1024
MAX_JSON_BYTES = 16 * 1024 * 1024
_ID = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")


def _word_ok(value: object) -> bool:
    """id・会話 ID・上流の短い語として台帳に書けるか（英数字と . _ : - の 1〜128 文字・鍵の形でない）。"""
    return isinstance(value, str) and bool(_ID.match(value)) and "sk-ant-" not in value.lower()


class Rejected(Exception):
    """送る前に拒む（status と Anthropic 形式の誤りの種類）。"""

    def __init__(self, status: int, etype: str, message: str):
        super().__init__(message)
        self.status = status
        self.etype = etype


def _invalid(message: str) -> Rejected:
    return Rejected(400, "invalid_request_error", f"abeval: {message}")


@dataclass
class Run:
    run_id: str
    exp: str
    cap_micros: int
    capture_only: bool = False


@dataclass
class Ticket:
    """送ってよいと決めた 1 回の要求（予約を持つ）。"""

    seq: int
    run: Run
    trial: str | None
    session: str | None
    reserved: int
    counting: bool
    model: str | None = None

    @property
    def tag_parts(self) -> tuple[str, str, str]:
        return self.run.exp, self.run.run_id, self.trial or "-"


def _check_id(name: str, value: object) -> str:
    if not _word_ok(value):
        raise _invalid(f"{name} は英数字と . _ : - の 1〜128 文字です")
    return value


class ProxyState:
    """実行の登録・試行の割り当て・予約・台帳（スレッドをまたいで 1 つの錠で守る）。"""

    def __init__(self, cfg: Config, *, upstream: str | None = None, capture: bool = False):
        self.cfg = cfg
        self.upstream = (upstream or cfg.proxy.upstream).rstrip("/")
        self.capture = capture
        self.ledger = Ledger(cfg.paths.ledger)
        self.token = secrets.token_urlsafe(32)
        self._lock = threading.Lock()
        self._runs: dict[str, Run] = {}
        self._sessions: dict[str, tuple[str, str]] = {}
        self._active: tuple[str, str | None] | None = None
        self._reserved: dict[int, tuple[str, int]] = {}
        self._seq = 0
        # 台帳に載らない失敗の数（実行ごと）。
        self._rejected: dict[str, int] = {}
        self._unreachable: dict[str, int] = {}
        self._last_rejection: dict[str, str] = {}
        self._unassigned_rejected = 0
        # 予約のある処理中の要求（止めるときに台帳に書く）と、書いた後の要求の番号。
        self._tickets: dict[int, Ticket] = {}
        self._abandoned: set[int] = set()
        self.ledger.refresh()  # 壊れた台帳なら起動しない。

    # --- 制御の口 ---

    def write_token(self) -> Path:
        home = self.cfg.paths.home
        home.mkdir(parents=True, exist_ok=True, mode=0o700)
        path = home / TOKEN_FILE
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            os.fchmod(fd, 0o600)
            os.write(fd, self.token.encode())
        finally:
            os.close(fd)
        return path

    def authorized(self, presented: str | None) -> bool:
        return presented is not None and hmac.compare_digest(
            presented.encode(), self.token.encode()
        )

    def register_run(self, body: dict) -> dict:
        run_id = _check_id("run_id", body.get("run_id"))
        exp = _check_id("exp", body.get("exp"))
        cap = body.get("cap_usd")
        if isinstance(cap, bool) or not isinstance(cap, int | float) or cap <= 0:
            raise _invalid("cap_usd は正の数です")
        capture_only = body.get("capture_only", False)
        if not isinstance(capture_only, bool):
            raise _invalid("capture_only は真偽値です")
        cap_micros = min(usd_to_micros(cap), self.cfg.caps.run_cap_micros)
        with self._lock:
            prev = self._runs.get(run_id)
            if prev is not None and prev.exp != exp:
                raise Rejected(
                    409, "invalid_request_error", "abeval: run_id は別の実験で登録済みです"
                )
            self._runs[run_id] = Run(run_id, exp, cap_micros, capture_only)
            spent = self.ledger.refresh().run_micros(run_id)
        return {
            "run_id": run_id,
            "exp": exp,
            "cap_micros": cap_micros,
            "spent_micros": spent,
            "capture_only": capture_only,
        }

    def remove_run(self, run_id: str) -> bool:
        with self._lock:
            if self._runs.pop(run_id, None) is None:
                return False
            self._sessions = {s: v for s, v in self._sessions.items() if v[0] != run_id}
            if self._active and self._active[0] == run_id:
                self._active = None
            return True

    def register_session(self, body: dict) -> dict:
        session = _check_id("session", body.get("session"))
        trial = _check_id("trial", body.get("trial"))
        run_id = _check_id("run_id", body.get("run_id"))
        with self._lock:
            if run_id not in self._runs:
                raise Rejected(404, "not_found_error", "abeval: 登録されていない run_id です")
            self._sessions[session.lower()] = (run_id, trial)
        return {"session": session, "trial": trial, "run_id": run_id}

    def set_active(self, body: dict) -> dict:
        run_id = body.get("run_id")
        with self._lock:
            if run_id is None:
                self._active = None
                return {"run_id": None, "trial": None}
            run_id = _check_id("run_id", run_id)
            trial = body.get("trial")
            trial = None if trial is None else _check_id("trial", trial)
            if run_id not in self._runs:
                raise Rejected(404, "not_found_error", "abeval: 登録されていない run_id です")
            self._active = (run_id, trial)
        return {"run_id": run_id, "trial": trial}

    def note_rejection(self, session: str | None, message: str) -> None:
        """送る前に拒んだ要求を数える（割り当てを決められなければ unassigned に数える）。"""
        with self._lock:
            try:
                run, _ = self._resolve(session)
            except Rejected:
                self._unassigned_rejected += 1
                return
            self._rejected[run.run_id] = self._rejected.get(run.run_id, 0) + 1
            self._last_rejection[run.run_id] = message[:300]

    def note_unreachable(self, ticket: Ticket) -> None:
        with self._lock:
            rid = ticket.run.run_id
            self._unreachable[rid] = self._unreachable.get(rid, 0) + 1

    def spend(self) -> dict:
        with self._lock:
            totals = self.ledger.refresh()
            reserved_by_run: dict[str, int] = {}
            for run_id, micros in self._reserved.values():
                reserved_by_run[run_id] = reserved_by_run.get(run_id, 0) + micros
            runs = {
                r.run_id: {
                    "exp": r.exp,
                    "cap_micros": r.cap_micros,
                    "spent_micros": totals.run_micros(r.run_id),
                    "reserved_micros": reserved_by_run.get(r.run_id, 0),
                    "capture_only": r.capture_only,
                    "rejected": self._rejected.get(r.run_id, 0),
                    "unreachable": self._unreachable.get(r.run_id, 0),
                    "last_rejection": self._last_rejection.get(r.run_id),
                }
                for r in self._runs.values()
            }
            return {
                "eval_spent_micros": totals.total_micros,
                "eval_cap_micros": self.cfg.caps.eval_cap_micros,
                "run_cap_micros": self.cfg.caps.run_cap_micros,
                "reserved_micros": sum(reserved_by_run.values()),
                "lines": totals.lines,
                "active": None
                if self._active is None
                else {"run_id": self._active[0], "trial": self._active[1]},
                "runs": runs,
                "unassigned_rejected": self._unassigned_rejected,
            }

    # --- 要求の受け付け ---

    def _resolve(self, session: str | None) -> tuple[Run, str | None]:
        if session:
            hit = self._sessions.get(session.lower())
            if hit and hit[0] in self._runs:
                return self._runs[hit[0]], hit[1]
        if self._active and self._active[0] in self._runs:
            return self._runs[self._active[0]], self._active[1]
        raise _invalid("登録された実行（run）に割り当たりません（会話 ID も active も無い）")

    def admit(self, req: object, body_bytes: int, session: str | None, counting: bool) -> Ticket:
        """送ってよいか決め、よければ最悪の額を予約する。"""
        if not isinstance(req, dict):
            raise _invalid("本文は JSON のオブジェクトです")
        model = req.get("model")
        if model not in self.cfg.proxy.allowed_models:
            raise _invalid(f"モデル {model!r} は許可リストにありません")
        sampling = [p for p in SAMPLING_PARAMS if p in req]
        if sampling:
            raise _invalid(
                f"{', '.join(sampling)} は送れません（Haiku 5.5 は 400・評価では送らない）"
            )
        with self._lock:
            run, trial = self._resolve(session)
            self._seq += 1
            seq = self._seq
            if counting or run.capture_only:
                return Ticket(seq, run, trial, session, 0, counting)
            max_tokens = req.get("max_tokens")
            if isinstance(max_tokens, bool) or not isinstance(max_tokens, int) or max_tokens <= 0:
                raise _invalid("max_tokens（正の整数）がありません")
            worst = self.cfg.pricing.reserve_micros(model, body_bytes, max_tokens)
            totals = self.ledger.refresh()
            reserved_all = sum(m for _, m in self._reserved.values())
            reserved_run = sum(m for r, m in self._reserved.values() if r == run.run_id)
            eval_cap = self.cfg.caps.eval_cap_micros
            if totals.total_micros + reserved_all + worst > eval_cap:
                raise _invalid(
                    "評価の予算の上限に達します（累計 "
                    f"${micros_to_usd(totals.total_micros):.4f}＋予約 ${micros_to_usd(reserved_all):.4f}"
                    f"＋この要求の最悪 ${micros_to_usd(worst):.4f} > ${micros_to_usd(eval_cap):.2f}）"
                )
            run_spent = totals.run_micros(run.run_id)
            if run_spent + reserved_run + worst > run.cap_micros:
                raise _invalid(
                    f"実行 {run.run_id} の上限に達します（累計 ${micros_to_usd(run_spent):.4f}＋予約 "
                    f"${micros_to_usd(reserved_run):.4f}＋この要求の最悪 ${micros_to_usd(worst):.4f} > "
                    f"${micros_to_usd(run.cap_micros):.2f}）"
                )
            self._reserved[seq] = (run.run_id, worst)
            ticket = Ticket(seq, run, trial, session, worst, counting, str(model))
            self._tickets[seq] = ticket
            return ticket

    def settle(self, ticket: Ticket, entry: dict | None) -> None:
        """台帳に書いてから予約を戻す（書いてから戻すので、累計が一瞬でも少なく見えない）。"""
        with self._lock:
            self._tickets.pop(ticket.seq, None)
            if ticket.seq in self._abandoned:
                self._abandoned.discard(
                    ticket.seq
                )  # 止めるときに予約の額で書いた（二重に書かない）
                return
            # 書けなかったら予約を残す（この中継が動いている間は、使ったかもしれない額を上限に数え続ける）。
            if entry is not None:
                self.ledger.append(entry)
            self._reserved.pop(ticket.seq, None)

    def abandon_inflight(self) -> int:
        """処理中の要求を予約の額で台帳に書く（止めるとき・書いた数）。後から終わっても二重に書かない。"""
        with self._lock:
            n = 0
            for seq, ticket in sorted(self._tickets.items()):
                if seq not in self._reserved:
                    continue
                exp, run_id, trial = ticket.tag_parts
                self.ledger.append(
                    {
                        "ts": _now(),
                        "exp": exp,
                        "run_id": run_id,
                        "trial": ticket.trial,
                        "session": ticket.session,
                        "model": ticket.model,
                        "path": MESSAGES,
                        "status": 0,
                        "usage": {},
                        "cost_micros": ticket.reserved,
                        "reserved_micros": ticket.reserved,
                        "usage_missing": True,
                        "client_gone": True,
                        "abandoned": True,
                        "captured": False,
                        "tag": f"{self.cfg.proxy.tag_prefix}/{exp}/{run_id}/{trial}",
                    }
                )
                self._reserved.pop(seq, None)
                self._abandoned.add(seq)
                n += 1
            return n

    def capture_dir(self, run_id: str) -> Path:
        return self.cfg.paths.cache / "requests" / run_id

    def save_request(self, ticket: Ticket, path: str, raw: bytes) -> Path:
        d = self.capture_dir(ticket.run.run_id)
        d.mkdir(parents=True, exist_ok=True)
        stem = f"{ticket.seq:06d}-{ticket.trial or '-'}"
        body_path = d / f"{stem}.json"
        body_path.write_bytes(raw)
        meta = {
            "path": path,
            "session": ticket.session,
            "trial": ticket.trial,
            "exp": ticket.run.exp,
            "ts": _now(),
            "req_sha256": hashlib.sha256(raw).hexdigest(),
        }
        (d / f"{stem}.meta.json").write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
        return body_path


def _safe_word(value: object) -> str | None:
    """上流が返した短い語（モデル・停止理由）を台帳に書ける形にする（形の違うものは書かない）。"""
    if value is None:
        return None
    return value if _word_ok(value) else "<unexpected>"


def _counts_only(usage: dict) -> dict:
    """usage のうち数だけ（入れ子の内訳も）を台帳に残す。"""
    out: dict = {}
    for k, v in usage.items():
        if not _word_ok(k) or "key" in k.lower() or "authorization" in k.lower():
            continue
        if isinstance(v, int | float) and not isinstance(v, bool):
            out[k] = v
        elif isinstance(v, dict):
            sub = _counts_only(v)
            if sub:
                out[k] = sub
    return out


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S%z")


def capture_sse(model: str) -> bytes:
    """capture_only で返す最小の正しい SSE（テキスト「abeval capture」で end_turn）。"""
    events = [
        (
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": "msg_abeval_capture",
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            },
        ),
        (
            "content_block_start",
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""},
            },
        ),
        (
            "content_block_delta",
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "abeval capture"},
            },
        ),
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        (
            "message_delta",
            {
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                "usage": {"output_tokens": 0},
            },
        ),
        ("message_stop", {"type": "message_stop"}),
    ]
    return b"".join(
        f"event: {name}\ndata: {json.dumps(data, ensure_ascii=False)}\n\n".encode()
        for name, data in events
    )


def capture_message(model: str) -> bytes:
    return json.dumps(
        {
            "id": "msg_abeval_capture",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": "abeval capture"}],
            "stop_reason": "end_turn",
            "stop_sequence": None,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }
    ).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"  # 応答は close で区切る（SSE をそのまま流す・手元の中継と同じ）
    timeout = 120
    server: ProxyServer

    def log_message(self, format, *args):  # 既定のログ（標準エラー）を抑える
        pass

    @property
    def state(self) -> ProxyState:
        return self.server.state

    # --- 応答の部品 ---

    def _json(self, status: int, payload: object) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode()
        self.send_response_only(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _error(self, status: int, etype: str, message: str) -> None:
        self._json(status, {"type": "error", "error": {"type": etype, "message": message}})

    def _forbidden(self) -> None:
        self._error(403, "permission_error", "abeval: この口は使えません")

    def _read_body(self) -> bytes:
        if self.headers.get("transfer-encoding"):
            raise Rejected(411, "invalid_request_error", "abeval: content-length が要ります")
        length = self.headers.get("content-length")
        if length is None or not length.isdigit():
            raise Rejected(411, "invalid_request_error", "abeval: content-length が要ります")
        n = int(length)
        if n > MAX_BODY_BYTES:
            raise Rejected(413, "request_too_large", "abeval: 本文が大きすぎます")
        return self.rfile.read(n)

    # --- 振り分け ---

    def do_POST(self):
        path = urlsplit(self.path).path
        if path in (MESSAGES, COUNT_TOKENS):
            return self._messages(path == COUNT_TOKENS)
        if path in ("/abeval/runs", "/abeval/sessions"):
            return self._control(
                self.state.register_run if path == "/abeval/runs" else self.state.register_session
            )
        return self._forbidden()

    def do_PUT(self):
        if urlsplit(self.path).path == "/abeval/active":
            return self._control(self.state.set_active, status=200)
        return self._forbidden()

    def do_DELETE(self):
        path = urlsplit(self.path).path
        prefix = "/abeval/runs/"
        if not path.startswith(prefix) or not self.state.authorized(
            self.headers.get(CONTROL_HEADER)
        ):
            return self._forbidden()
        run_id = path[len(prefix) :]
        if not _word_ok(run_id):
            return self._error(400, "invalid_request_error", "abeval: run_id が不正です")
        if not self.state.remove_run(run_id):
            return self._error(404, "not_found_error", "abeval: 登録されていない run_id です")
        self.send_response_only(204)
        self.end_headers()

    def do_GET(self):
        if urlsplit(self.path).path == "/abeval/spend" and self.state.authorized(
            self.headers.get(CONTROL_HEADER)
        ):
            return self._json(200, self.state.spend())
        return self._forbidden()

    def do_PATCH(self):
        self._forbidden()

    def do_HEAD(self):
        self._forbidden()

    def do_OPTIONS(self):
        self._forbidden()

    def _control(self, action, status: int = 201) -> None:
        if not self.state.authorized(self.headers.get(CONTROL_HEADER)):
            return self._forbidden()
        try:
            body = json.loads(self._read_body() or b"{}")
            if not isinstance(body, dict):
                raise _invalid("本文は JSON のオブジェクトです")
            result = action(body)
        except Rejected as e:
            return self._error(e.status, e.etype, str(e))
        except ValueError:
            return self._error(400, "invalid_request_error", "abeval: 本文が JSON ではありません")
        self._json(status, result)

    # --- Messages ---

    def _messages(self, counting: bool) -> None:
        state = self.state
        try:
            raw = self._read_body()
            try:
                req = json.loads(raw)
            except ValueError:
                raise _invalid("本文が JSON ではありません") from None
            session = self.headers.get(state.cfg.proxy.session_header)
            if session is not None and not _word_ok(session):
                raise _invalid(f"{state.cfg.proxy.session_header} の値が不正です")
            ticket = state.admit(req, len(raw), session, counting)
        except Rejected as e:
            if not counting:
                state.note_rejection(self.headers.get(state.cfg.proxy.session_header), str(e))
            return self._error(e.status, e.etype, str(e))
        if ticket.run.capture_only:
            return self._capture(ticket, raw, req)
        if state.capture and not counting:
            state.save_request(ticket, self.path, raw)
        self._forward(ticket, raw, req)

    def _capture(self, ticket: Ticket, raw: bytes, req: dict) -> None:
        """上流へ送らずに本文を保存して最小の応答を返す（台帳には 0 で書いてから返す）。"""
        state = self.state
        entry = None
        try:
            if not ticket.counting:
                state.save_request(ticket, self.path, raw)
                entry = self._entry(ticket, raw, req, 200, 0.0, MessageMeta(), 0, captured=True)
        finally:
            state.settle(ticket, entry)
        model = req.get("model", "")
        if ticket.counting:
            return self._json(200, {"input_tokens": 0})
        stream = bool(req.get("stream"))
        body = capture_sse(model) if stream else capture_message(model)
        self.send_response_only(200)
        if stream:
            self.send_header("content-type", "text/event-stream")
            self.send_header("cache-control", "no-cache")
        else:
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _upstream_headers(self, ticket: Ticket) -> dict[str, str]:
        headers = {k: v for k in PASS_HEADERS if (v := self.headers.get(k)) is not None}
        headers["x-api-key"] = DUMMY_CREDENTIAL
        headers["accept-encoding"] = "identity"
        headers["connection"] = "close"  # 上流（手元の中継）は HTTP/1.0 の close 区切り
        exp, run_id, trial = ticket.tag_parts
        headers["x-campaign-tag"] = f"{self.state.cfg.proxy.tag_prefix}/{exp}/{run_id}/{trial}"
        return headers

    def _forward(self, ticket: Ticket, raw: bytes, req: dict) -> None:
        state = self.state
        url = state.upstream + self.path
        started = time.monotonic()
        status = 0
        sent_to_upstream = False
        stream_done = False
        client_gone = False
        sse: SseReader | None = None
        body = bytearray()
        try:
            with (
                httpx.Client(
                    timeout=httpx.Timeout(connect=10.0, read=900.0, write=120.0, pool=10.0),
                    limits=httpx.Limits(max_keepalive_connections=0),
                    trust_env=False,
                ) as client,
                client.stream(
                    "POST", url, content=raw, headers=self._upstream_headers(ticket)
                ) as resp,
            ):
                sent_to_upstream = True
                status = resp.status_code
                self.send_response_only(status)
                for k, v in resp.headers.multi_items():
                    if k.lower() not in HOP_HEADERS:
                        self.send_header(k, v)
                self.end_headers()
                if "text/event-stream" in resp.headers.get("content-type", ""):
                    sse = SseReader()
                for chunk in resp.iter_raw():
                    if not chunk:
                        continue
                    try:
                        self.wfile.write(chunk)
                        self.wfile.flush()
                    except OSError:
                        client_gone = True  # 呼び出し側が切った・書けない（上流は閉じて止める）
                        break
                    if sse is not None:
                        sse.feed(chunk)
                    elif len(body) < MAX_JSON_BYTES:
                        body.extend(chunk)
                else:
                    stream_done = True
        except (httpx.ConnectError, httpx.ConnectTimeout) as e:
            # 上流に届いていない（費用は無い・台帳に載らないので数える）。
            state.note_unreachable(ticket)
            self._safe_error(502, "api_error", f"abeval: 上流に繋がりません: {type(e).__name__}")
        except httpx.HTTPError as e:
            sent_to_upstream = True  # 送った後の誤り（上流で費用が出たかもしれない）
            if status == 0:
                self._safe_error(502, "api_error", f"abeval: 上流の誤り: {type(e).__name__}")
        except OSError:
            client_gone = True
        finally:
            if status == 0 and not sent_to_upstream:
                state.settle(ticket, None)
            else:
                meta = sse.finish() if sse is not None else read_message(bytes(body))
                secs = time.monotonic() - started
                if ticket.counting:
                    state.settle(ticket, None)  # count_tokens は台帳に載せない
                else:
                    cost, missing = self._cost(ticket, req, status, meta, stream_done)
                    entry = self._entry(
                        ticket,
                        raw,
                        req,
                        status,
                        secs,
                        meta,
                        cost,
                        missing=missing,
                        gone=client_gone,
                    )
                    state.settle(ticket, entry)

    def _safe_error(self, status: int, etype: str, message: str) -> None:
        with contextlib.suppress(OSError):
            self._error(status, etype, message)

    def _cost(
        self, ticket: Ticket, req: dict, status: int, meta: MessageMeta, stream_done: bool
    ) -> tuple[int, bool]:
        """(費用, usage_missing)。誤りの応答を読み切ったものは 0、最後の usage が無ければ予約の額。"""
        if 400 <= status < 600 and stream_done and not meta.complete:
            return 0, False
        if meta.complete and stream_done:
            model = req.get("model", "")
            return self.state.cfg.pricing.cost_micros(model, meta.usage), False
        return ticket.reserved, True

    def _entry(
        self,
        ticket: Ticket,
        raw: bytes,
        req: dict,
        status: int,
        secs: float,
        meta: MessageMeta,
        cost: int,
        *,
        missing: bool = False,
        gone: bool = False,
        captured: bool = False,
    ) -> dict:
        exp, run_id, trial = ticket.tag_parts
        return {
            "ts": _now(),
            "exp": exp,
            "run_id": run_id,
            "trial": ticket.trial,
            "session": ticket.session,
            "model": req.get("model"),
            "resp_model": _safe_word(meta.model),
            "path": urlsplit(self.path).path,
            "status": status,
            "secs": round(secs, 3),
            "usage": _counts_only(meta.usage),
            "stop_reason": _safe_word(meta.stop_reason),
            "cost_micros": cost,
            "reserved_micros": ticket.reserved,
            "usage_missing": missing,
            "client_gone": gone,
            "captured": captured,
            "req_bytes": len(raw),
            "req_sha256": hashlib.sha256(raw).hexdigest(),
            "tag": f"{self.state.cfg.proxy.tag_prefix}/{exp}/{run_id}/{trial}",
        }


class ProxyServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, state: ProxyState, host: str, port: int):
        if not is_loopback_host(host):
            raise ValueError(f"abeval.proxy はループバックにだけ bind します: {host!r}")
        if ":" in host:
            self.address_family = socket.AF_INET6
        self.state = state
        super().__init__((host.strip("[]"), port), Handler)

    @property
    def url(self) -> str:
        host, port = self.server_address[:2]
        return f"http://[{host}]:{port}" if ":" in str(host) else f"http://{host}:{port}"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="abeval.proxy", description=__doc__.splitlines()[0])
    ap.add_argument("--listen", help="host:port（既定は eval.toml の [proxy] listen）")
    ap.add_argument("--upstream", help="上流の URL（既定は eval.toml の [proxy] upstream）")
    ap.add_argument("--capture", action="store_true", help="転送した要求の本文も保存する")
    args = ap.parse_args(argv)
    cfg = load_config()
    host, port = cfg.proxy.host, cfg.proxy.port
    if args.listen:
        h, _, p = args.listen.rpartition(":")
        host, port = h.strip("[]"), int(p)
    from abeval.budget import ledger_guard

    why = ledger_guard(cfg)
    if why:
        print(f"abeval.proxy: 起動しません: {why}", file=sys.stderr, flush=True)
        return 2
    state = ProxyState(cfg, upstream=args.upstream, capture=args.capture)
    server = ProxyServer(state, host, port)
    token_path = state.write_token()
    spent = state.ledger.refresh().total_micros
    print(
        f"abeval.proxy: {server.url} → {state.upstream}  評価の累計 ${micros_to_usd(spent):.4f} / "
        f"${cfg.caps.eval_cap_usd:.2f}  台帳 {display_path(cfg.paths.ledger)}  "
        f"合言葉 {display_path(token_path)}",
        file=sys.stderr,
        flush=True,
    )

    def stop(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        n = state.abandon_inflight()
        if n:
            print(
                f"abeval.proxy: 処理中の要求 {n} 件を予約の額で台帳に書きました（usage_missing）",
                file=sys.stderr,
                flush=True,
            )
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
