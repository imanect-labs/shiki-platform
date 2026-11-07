"""偽の shiki に、台本どおりに動くチャットと、製品の Anthropic への要求のまね（取り込み用）を足す。

- `RunScript`: 1 回の発話（run）の動き。SSE のイベント（`events`）・終わったときに足すメッセージ（`messages`）・
  終わるまでに `GET messages` が active を返す回数（`polls_active`）・終わらない（`hang`・取り消しで終わる）・
  SSE を空で返す（`stream_empty`・どの接続も空）・最初の何回かの接続だけ空（`empty_connects`・繋いだときに
  まだ何も流れていなかった）・最初の中身のある接続を何件かのイベントの後で切る（`cut_after`）・終わりのイベントを
  足さない（`auto_done=False`）。
- SSE は製品と同じく、スレッドの「いまの run」（終わっていない run、無ければ最新の run）のイベントのうち
  `last_event_id` より後を流す（終わった run なら残りを流して閉じる・chat.rs の stream_thread と store/stream.rs）。
  台本に終わりのイベント（done・error・status の done・failed・cancelled）が無ければ `status: done` を最後に足す。
- 発話を受けると、製品と同じく中身の無い assistant の発話（`content: []`）を置き、run が終わったら台本の
  メッセージで置き換える（台本のメッセージが無ければ中身の無いまま残る）。
- `ChatFake.llm_url` があれば、発話を受けたときに製品と同じ形の要求（system の末尾に「今日の日付」・tools に
  emit_workflow〔defer_loading〕と data のツール〔見えるテーブルの名前と id の一覧〕）を `{llm_url}/v1/messages` に
  会話 ID（thread_id）のヘッダーを付けて送る（abeval.proxy の capture_only で受ける・上流へは出ない）。
"""

from __future__ import annotations

import json
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field

import httpx

from fake_shiki import FakeShiki, _json

TODAY_NOTE = "\n\n今日は 2026年11月6日（金）です（Asia/Tokyo）。「今日」「来週」「今月」などはこの日付を基準に解釈してください。"
SYSTEM = "あなたは社内の業務を手伝うアシスタントです。"


TERMINAL_STATUSES = ("done", "failed", "cancelled")


@dataclass
class RunScript:
    events: list[dict] = field(default_factory=list)
    messages: list[dict] = field(default_factory=list)
    polls_active: int = 0
    hang: bool = False
    stream_empty: bool = False
    keepalive: bool = True
    # 終わったときに呼ぶ（中継の台帳に行を足す・会計を増やすなど・引数は thread_id）。
    on_done: Callable[[str], None] | None = None
    # 最初の何回かの SSE の接続は何も流さない（繋いだときにまだ何も無かった・空で閉じた）。
    empty_connects: int = 0
    # 最初の中身のある接続を、この数のイベントの後で切る（httpx.ReadError）。後の接続は続きから流す。
    cut_after: int | None = None
    # 台本に終わりのイベントが無ければ `status: done` を足す（製品の run は必ず終わりのイベントで閉じる）。
    auto_done: bool = True

    def stream_events(self) -> list[dict]:
        if self.auto_done and not any(_terminal(e) for e in self.events):
            return [*self.events, {"type": "status", "status": "done"}]
        return list(self.events)


def _terminal(e: dict) -> bool:
    return e.get("type") in ("done", "error") or (
        e.get("type") == "status" and e.get("status") in TERMINAL_STATUSES
    )


def text_message(text: str, *blocks: dict) -> dict:
    return {"role": "assistant", "content": [*blocks, {"type": "text", "text": text}]}


def sse_text(events: list[dict], keepalive: bool = True, start: int = 1) -> str:
    out = [":\n\n"] if keepalive else []
    for i, e in enumerate(events, start):
        out.append(f"id: {i}\ndata: {json.dumps(e, ensure_ascii=False)}\n\n")
    return "".join(out)


class _CutStream(httpx.SyncByteStream):
    """途中まで流して切れる SSE（`httpx.ReadError`）。"""

    def __init__(self, text: str):
        self.text = text

    def __iter__(self):
        yield self.text.encode()
        raise httpx.ReadError("SSE が途中で切れた（偽）")


@dataclass
class ChatFake(FakeShiki):
    scripts: list[RunScript] = field(default_factory=list)
    llm_url: str | None = None
    llm_replies: list[int] = field(default_factory=list)
    posted: list[dict] = field(default_factory=list)
    approvals: list[dict] = field(default_factory=list)
    cancels: list[str] = field(default_factory=list)
    streams: int = 0
    gets: int = 0
    usage_micros: int = 0
    # /workflows/validate の誤り（IR → ValidationError の列）。無ければ偽の shiki の既定（誤りなし）。
    validator: Callable[[dict], list[dict]] | None = None

    def product_request(self, thread_id: str, text: str, body: dict) -> dict:
        listing = "".join(
            f"- {t['name']} / {t['id']}\n" for t in self.tables.values() if t["tenant"] == "a-corp"
        )
        tools = [
            {
                "name": "tool_search",
                "description": "道具を探す",
                "input_schema": {"type": "object"},
            },
            {
                "name": "emit_workflow",
                "description": "ワークフローの IR を保存する。",
                "input_schema": {"type": "object", "properties": {"ir": {"type": "object"}}},
                "defer_loading": True,
            },
            {
                "name": "read_workflow",
                "description": "保存したワークフローを読む",
                "input_schema": {"type": "object"},
                "defer_loading": True,
            },
        ]
        if listing:
            tools.append(
                {
                    "name": "data_query",
                    "description": "テーブルを読む。\n\n閲覧できるテーブル（名前 / id）:\n"
                    + listing,
                    "input_schema": {"type": "object"},
                    "defer_loading": True,
                }
            )
        req = {
            "model": body.get("model") or "claude-haiku-5-5",
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": text}],
            "stream": True,
            "system": SYSTEM + TODAY_NOTE,
            "tools": tools,
        }
        if body.get("effort"):
            req["thinking"] = {"type": "adaptive"}
            req["output_config"] = {"effort": body["effort"]}
        return req

    def _send_to_llm(self, thread_id: str, text: str, body: dict) -> None:
        req = self.product_request(thread_id, text, body)
        # 題名の生成（ツールの無い要求）も同じ会話 ID で先に送る（取り込みはツールのある要求を選ぶ）。
        title = {k: v for k, v in req.items() if k != "tools"}
        title.update({"system": "題名を付ける", "max_tokens": 64})
        with httpx.Client(trust_env=False, timeout=10) as c:
            for r in (title, req):
                resp = c.post(
                    f"{self.llm_url}/v1/messages",
                    json=r,
                    headers={
                        "x-api-key": "abeval-dummy",
                        "anthropic-version": "2023-06-01",
                        "x-abeval-session": thread_id,
                    },
                )
                self.llm_replies.append(resp.status_code)

    def _api(self, request: httpx.Request, user: str, tenant: str) -> httpx.Response:
        parts = request.url.path.strip("/").split("/")
        m = request.method
        if parts[0] == "workflows" and len(parts) == 4 and parts[2] == "versions" and m == "GET":
            w = self.workflows.get(parts[1])
            if w is None or w["tenant"] != tenant:
                return _json(404, {})
            return _json(200, {"id": w["id"], "version": int(parts[3]), "ir": w["ir"]})
        if request.url.path == "/workflows/validate" and m == "POST" and self.validator:
            errs = self.validator(json.loads(request.content)["ir"])
            return _json(
                200, {"errors": errs, "warnings": [], "declared_scopes": [], "node_ports": {}}
            )
        if request.url.path == "/org-admin/ai/usage":
            return _json(200, {"totals": {"cost_usd_micros": self.usage_micros, "calls": 0}})
        if parts[0] != "threads" or len(parts) < 3:
            return super()._api(request, user, tenant)
        th = self.threads.get(parts[1])
        if th is None or th["tenant"] != tenant:
            return _json(404, {})
        body = json.loads(request.content) if request.content else {}
        if parts[2:] == ["messages"] and m == "POST":
            self.posted.append(body)
            run_id = str(uuid.uuid4())
            script = (
                self.scripts.pop(0)
                if self.scripts
                else RunScript(messages=[text_message(f"回答: {body['text']}")])
            )
            placeholder = {"role": "assistant", "content": []}
            th.setdefault("runs", {})[run_id] = {
                "script": script,
                "polls": 0,
                "connects": 0,
                "placeholder": placeholder,
            }
            th["messages"].append(
                {"role": "user", "content": [{"type": "text", "text": body["text"]}]}
            )
            th["messages"].append(placeholder)
            th["active"] = run_id
            th["latest"] = run_id
            if self.llm_url:
                self._send_to_llm(th["id"], body["text"], body)
            return _json(
                202, {"run_id": run_id, "user_message_id": "u", "assistant_message_id": "a"}
            )
        if parts[2:] == ["messages"] and m == "GET":
            self.gets += 1
            run_id = th.get("active")
            if run_id:
                run = th["runs"][run_id]
                run["polls"] += 1
                if not run["script"].hang and run["polls"] > run["script"].polls_active:
                    if run["script"].messages:
                        th["messages"] = [m for m in th["messages"] if m is not run["placeholder"]]
                    th["messages"] += run["script"].messages
                    th["active"] = None
                    if run["script"].on_done:
                        run["script"].on_done(th["id"])
            page = {"messages": th["messages"]}
            if th.get("active"):
                page["active_run_id"] = th["active"]
            return _json(200, page)
        if parts[2:] == ["stream"]:
            self.streams += 1
            # いまの run（終わっていない run、無ければ最新の run）の last_event_id より後のイベント。
            run_id = th.get("active") or th.get("latest")
            headers = [("content-type", "text/event-stream")]
            if not run_id:
                return httpx.Response(200, text="", headers=headers)
            run = th["runs"][run_id]
            s = run["script"]
            run["connects"] += 1
            if s.stream_empty or run["connects"] <= s.empty_connects:
                return httpx.Response(200, text="", headers=headers)
            last = int(request.url.params.get("last_event_id") or 0)
            events = s.stream_events()[last:]
            if s.cut_after is not None and not run.get("cut"):
                run["cut"] = True
                text = sse_text(events[: s.cut_after], s.keepalive, last + 1)
                return httpx.Response(200, stream=_CutStream(text), headers=headers)
            return httpx.Response(
                200, text=sse_text(events, s.keepalive, last + 1), headers=headers
            )
        if len(parts) == 5 and parts[2] == "runs" and parts[4] == "approvals":
            run = th.get("runs", {}).get(parts[3])
            if run is None:
                return _json(404, {})
            want = {
                e["tool_call_id"]: e["name"]
                for e in run["script"].events
                if e.get("type") == "approval_requested"
            }
            if want.get(body.get("tool_call_id")) != body.get("tool_name"):
                return _json(400, {"title": "tool_name が承認の要求と違います"})
            self.approvals.append({"run_id": parts[3], **body})
            return httpx.Response(204)
        if len(parts) == 5 and parts[2] == "runs" and parts[4] == "cancel":
            self.cancels.append(parts[3])
            if th.get("active") == parts[3]:
                th["active"] = None
            return httpx.Response(204)
        return super()._api(request, user, tenant)
