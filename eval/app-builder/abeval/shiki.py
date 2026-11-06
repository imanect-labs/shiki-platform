"""shiki の API クライアント（評価の版のサーバー・stub のスタックを製品の公開の口で叩く・GAP-30 §4.4）。

- 利用者ごとに 1 つ（Cookie の入れ物を分ける）。BFF（:3000/api）は通さず、API を直接叩く。
- ログインは `deploy/compose/smoke-bff.sh` と同じ OIDC の手順: `GET /auth/login` → Keycloak の画面の form action
  （`&amp;` を戻す）→ username と password を POST → 302 の Location の scheme・host・port を API の基底に置き換えて
  （path と query はそのまま）`GET /auth/callback`（302 は追わない）。dev-up の既定は戻り先が :3000 なので、
  書き換えて API に届ける。Keycloak の Cookie はアプリの Cookie と分けて持つ（ログインのたびに作り直す）。
- 状態を変える要求（POST・PUT・PATCH・DELETE）にだけ `x-csrf-token` に Cookie `shiki_csrf` の値を付ける。
- 401 なら 1 回だけログインし直して送り直す。
- `stub_only=True`（stub のスタック）では、実モデルを指す発話と IR を送る前に止める（[`assert_stub_only`]）。
  手元の llm-stub.toml の anthropic は 127.0.0.1:8787（本物の中継）を指しているため。
- 製品の規則（検証・整形）は写さない。応答の知らない項目は無視する（版の間で形が変わる）。
"""

from __future__ import annotations

import html
import json
import re
import time
from collections.abc import Iterator, Mapping
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import httpx

DEFAULT_PASSWORD = "password"
# dev seed の利用者と組織（crates/api/src/dev_seed.rs）。
SEED_ORGS = {"alice": "a-corp", "bob": "a-corp", "charlie": "b-corp"}
SESSION_COOKIE = "shiki_session"
CSRF_COOKIE = "shiki_csrf"
CSRF_HEADER = "x-csrf-token"
STATE_CHANGING = frozenset({"POST", "PUT", "PATCH", "DELETE"})
# stub のスタックで許すモデル（チャットの発話は default だけ・IR は default と判定の decide-stub）。
STUB_CHAT_MODELS = frozenset({"default"})
STUB_IR_MODELS = frozenset({"default", "decide-stub"})
# モデルを省くと組織の既定のモデルで動くノード（stub の守りでは省略を許さない）。
LLM_NODE_TYPES = frozenset({"llm.invoke", "ai.decide", "agent.invoke"})
_REAL_MODEL_HINT = re.compile(r"claude|haiku|sonnet|opus|gpt-|gemini|deepseek", re.IGNORECASE)
_FORM_LOGIN = re.compile(r'<form[^>]*\bid="kc-form-login"[^>]*>', re.IGNORECASE)
_ACTION = re.compile(r'\baction="([^"]+)"')
TERMINAL_RUN = frozenset({"succeeded", "failed", "cancelled"})


class ShikiError(RuntimeError):
    """API が期待しない状態を返した。"""

    def __init__(self, method: str, path: str, status: int, body: str):
        self.method, self.path, self.status, self.body = method, path, status, body
        super().__init__(f"{method} {path} → {status}: {body[:300]}")


class LoginError(RuntimeError):
    """OIDC のログインの手順が途中で止まった（画面の一部を添える）。"""


class StubOnlyViolation(ValueError):
    """stub のスタックへ実モデルを指す発話・IR を送ろうとした。"""


# --- stub の守り ---


def _walk(value: Any, at: str = "") -> Iterator[tuple[str, Any]]:
    yield at, value
    if isinstance(value, Mapping):
        for k, v in value.items():
            yield from _walk(v, f"{at}.{k}" if at else str(k))
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            yield from _walk(v, f"{at}[{i}]")


def _check_ir(ir: Mapping) -> list[str]:
    bad = []
    for at, v in _walk(ir):
        if isinstance(v, Mapping):
            model = v.get("model")
            if isinstance(model, str) and model and model not in STUB_IR_MODELS:
                bad.append(f"{at}.model={model!r}")
            if v.get("type") in LLM_NODE_TYPES:
                params = v.get("params")
                m = params.get("model") if isinstance(params, Mapping) else None
                if not isinstance(m, str) or not m:
                    bad.append(f"{at}（{v.get('type')}）の model を省いている（組織の既定で動く）")
        elif isinstance(v, str) and _REAL_MODEL_HINT.search(v):
            bad.append(f"{at} に実モデルの名前: {v[:60]!r}")
    return bad


def assert_stub_only(obj: Mapping) -> None:
    """stub のスタックへ送ってよい IR（`nodes` を持つ）か発話（`text` を持つ）か確かめる。"""
    if not isinstance(obj, Mapping):
        raise StubOnlyViolation(f"IR か発話の本文ではありません: {type(obj).__name__}")
    if "nodes" in obj:
        bad = _check_ir(obj)
    elif "text" in obj:
        bad = []
        model = obj.get("model")
        if model not in STUB_CHAT_MODELS:
            bad.append(
                f"発話の model は {sorted(STUB_CHAT_MODELS)} のどれかを明示します: {model!r}"
            )
        bad += [x for x in _check_ir({k: v for k, v in obj.items() if k != "model"})]
    else:
        raise StubOnlyViolation("IR（nodes）でも発話（text）でもありません")
    if bad:
        raise StubOnlyViolation("stub のスタックには送りません: " + "; ".join(bad))


# --- SSE ---


KEEPALIVE = {"type": "_keepalive"}


def iter_sse(lines: Iterator[str], *, keepalive: bool = False) -> Iterator[dict]:
    """SSE の行からイベント（{"id": int|None, "data": <JSON>}）を読む。コメント（:）と空の data は飛ばす。

    `keepalive=True` なら、コメントの行（サーバーの keep-alive）ごとに `{"id": None, "data": KEEPALIVE}` を返す
    （イベントの来ない間も、呼び出し側が壁時計を見られるように）。
    """
    data: list[str] = []
    event_id: int | None = None
    for raw in lines:
        line = raw.rstrip("\r")
        if not line:
            if data:
                text = "\n".join(data)
                try:
                    payload = json.loads(text)
                except ValueError:
                    payload = {"type": "_unparsed", "raw": text}
                yield {"id": event_id, "data": payload}
            data, event_id = [], None
            continue
        if line.startswith(":"):
            if keepalive and not data:
                yield {"id": None, "data": dict(KEEPALIVE)}
            continue
        name, _, value = line.partition(":")
        value = value[1:] if value.startswith(" ") else value
        if name == "data":
            data.append(value)
        elif name == "id":
            event_id = int(value) if value.isdigit() else None
    if data:
        try:
            yield {"id": event_id, "data": json.loads("\n".join(data))}
        except ValueError:
            yield {"id": event_id, "data": {"type": "_unparsed", "raw": "\n".join(data)}}


# --- クライアント ---


class ShikiClient:
    def __init__(
        self,
        api_url: str,
        user: str,
        password: str = DEFAULT_PASSWORD,
        *,
        transport: httpx.BaseTransport | None = None,
        timeout: float = 30.0,
        stub_only: bool = False,
    ):
        self.api = api_url.rstrip("/")
        parts = urlsplit(self.api)
        if parts.scheme not in ("http", "https") or not parts.netloc:
            raise ValueError(f"API の URL ではありません: {api_url!r}")
        self.user = user
        self._password = password
        self._transport = transport
        self._timeout = timeout
        self.stub_only = stub_only
        self.logins = 0
        self._http = httpx.Client(
            base_url=self.api, transport=transport, timeout=timeout, follow_redirects=False
        )

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> ShikiClient:
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # --- ログイン ---

    def _cookie(self, name: str) -> str | None:
        for c in self._http.cookies.jar:
            if c.name == name:
                return c.value
        return None

    @property
    def logged_in(self) -> bool:
        return self._cookie(SESSION_COOKIE) is not None

    def login(self) -> dict:
        """OIDC の手順でログインし、`/me` を返す。"""
        self._http.cookies.clear()
        r = self._http.get("/auth/login")
        authorize = r.headers.get("location")
        if r.status_code not in (302, 303, 307) or not authorize:
            raise LoginError(f"/auth/login が authorize へ飛ばしません（{r.status_code}）")
        with httpx.Client(
            transport=self._transport, timeout=self._timeout, follow_redirects=False
        ) as kc:
            jar = _KeycloakJar(authorize)
            page = jar.send(kc, "GET", authorize)
            hops = 0
            while (
                page.status_code in (302, 303)
                and hops < 5
                and "code=" not in page.headers.get("location", "")
            ):
                page = jar.send(kc, "GET", httpx.URL(str(page.url)).join(page.headers["location"]))
                hops += 1
            if page.status_code in (302, 303):
                callback = page.headers["location"]  # 既に Keycloak のセッションがある
            else:
                action = self._form_action(page)
                done = jar.send(
                    kc,
                    "POST",
                    httpx.URL(str(page.url)).join(action),
                    data={"username": self.user, "password": self._password},
                )
                callback = done.headers.get("location", "")
                if done.status_code not in (302, 303) or "code=" not in callback:
                    raise LoginError(
                        f"{self.user} の資格情報の POST が戻り先へ飛ばしません（{done.status_code}）: "
                        + _snippet(done.text)
                    )
        target = self._to_api(callback)
        cb = self._http.get(target)
        if cb.status_code not in (302, 303) or not self.logged_in or not self._cookie(CSRF_COOKIE):
            raise LoginError(
                f"/auth/callback でセッションができません（{cb.status_code}）: {_snippet(cb.text)}"
            )
        self.logins += 1
        # ここで 401 なら、もう一度ログインせずに誤りにする（再ログインの往復を重ねない）。
        return self._json("GET", "/me", relogin=False)

    @staticmethod
    def _form_action(page: httpx.Response) -> str:
        text = page.text
        form = _FORM_LOGIN.search(text)
        hit = _ACTION.search(form.group(0)) if form else _ACTION.search(text)
        if not hit:
            raise LoginError(
                f"Keycloak のログイン画面から form action を取れません（{page.status_code}）: "
                + _snippet(text)
            )
        return html.unescape(hit.group(1))

    def _to_api(self, location: str) -> str:
        """戻り先の scheme・host・port を API の基底に置き換える（path と query はそのまま）。"""
        loc = urlsplit(location)
        api = urlsplit(self.api)
        return urlunsplit((api.scheme, api.netloc, loc.path, loc.query, ""))

    # --- 送る ---

    def request(
        self,
        method: str,
        path: str,
        *,
        json_body: Any = None,
        params: Mapping | None = None,
        ok: tuple[int, ...] | None = None,
        auth: bool = True,
        relogin: bool = True,
    ) -> httpx.Response:
        """送って応答を返す。`ok` が無ければ 2xx を成功とし、それ以外は ShikiError。

        401 なら 1 回だけログインし直して送り直す（`relogin=False` なら送り直さない）。
        """
        method = method.upper()
        if auth and not self.logged_in:
            self.login()
        for attempt in (0, 1):
            headers = {}
            if auth and method in STATE_CHANGING:
                headers[CSRF_HEADER] = self._cookie(CSRF_COOKIE) or ""
            r = self._http.request(method, path, json=json_body, params=params, headers=headers)
            if r.status_code == 401 and auth and relogin and attempt == 0:
                self.login()
                continue
            break
        good = r.status_code in ok if ok is not None else 200 <= r.status_code < 300
        if not good:
            raise ShikiError(method, path, r.status_code, r.text)
        return r

    def _json(self, method: str, path: str, **kw) -> Any:
        r = self.request(method, path, **kw)
        return r.json() if r.content else None

    # --- 公開の口・本人 ---

    def healthz(self) -> int:
        return self._http.get("/healthz").status_code

    def openapi(self) -> dict:
        return self._json("GET", "/api-docs/openapi.json", auth=False)

    def me(self) -> dict:
        return self._json("GET", "/me")

    def custom_instructions(self) -> dict | None:
        """カスタム指示（無い版は 404 → None）。"""
        r = self.request("GET", "/me/custom-instructions", ok=(200, 404))
        return None if r.status_code == 404 else r.json()

    def llm_models(self, purpose: str | None = None, modality: str | None = None) -> dict:
        params = {k: v for k, v in (("purpose", purpose), ("modality", modality)) if v}
        return self._json("GET", "/llm/models", params=params or None)

    def usage(self, *, from_: str | None = None, to: str | None = None, **filters) -> dict:
        params = {"from": from_, "to": to, **filters}
        return self._json(
            "GET", "/org-admin/ai/usage", params={k: v for k, v in params.items() if v}
        )

    def capabilities(self, captured_tools: list[str] | None = None) -> dict:
        """版の機能の有無（OpenAPI から）。チャットの道具の有無は取り込んだツールの一覧で決める。"""
        doc = self.openapi()
        return capabilities_of(doc, captured_tools)

    # --- ワークフロー ---

    def _guard(self, obj: Mapping) -> None:
        if self.stub_only:
            assert_stub_only(obj)

    def validate(self, ir: Mapping) -> dict:
        return self._json("POST", "/workflows/validate", json_body={"ir": ir})

    def create_workflow(self, ir: Mapping) -> dict:
        self._guard(ir)
        return self._json("POST", "/workflows", json_body={"ir": ir}, ok=(201,))

    def update_workflow(self, wf_id: str, ir: Mapping, expected_version: int | None = None) -> dict:
        self._guard(ir)
        body: dict = {"ir": ir}
        if expected_version is not None:
            body["expected_version"] = expected_version
        return self._json("PUT", f"/workflows/{wf_id}", json_body=body)

    def get_workflow(self, wf_id: str) -> dict:
        return self._json("GET", f"/workflows/{wf_id}")

    def get_workflow_version(self, wf_id: str, version: int) -> dict:
        return self._json("GET", f"/workflows/{wf_id}/versions/{version}")

    def workflows(self) -> list[dict]:
        """見えるワークフロー全部（自分のものと共有された分の続きを読む）。"""
        items: list[dict] = []
        params: dict = {"limit": 100}
        while True:
            page = self._json("GET", "/workflows", params=params)
            items += page.get("items", [])
            nxt = page.get("owned_next")
            if nxt:
                params = {"limit": 100, **nxt}
                continue
            shared = page.get("shared_next_cursor")
            if shared:
                params = {"limit": 100, "shared_cursor": shared}
                continue
            return items

    def delete_artifact(self, artifact_id: str, *, missing_ok: bool = False) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        return self.request("DELETE", f"/artifacts/{artifact_id}", ok=ok).status_code == 204

    def start_run(
        self,
        wf_id: str,
        input: Mapping | None = None,
        *,
        trigger: Mapping | None = None,
        scheduled_at: str | None = None,
    ) -> str | None:
        """対話の起動。run_id（起動しなかったときは None）。"""
        if self.stub_only:
            self._guard(self.get_workflow(wf_id).get("ir") or {})
        body: dict = {"input": dict(input or {})}
        if trigger is not None:
            body["trigger"] = dict(trigger)
        if scheduled_at is not None:
            body["scheduled_at"] = scheduled_at
        return self._json("POST", f"/workflows/{wf_id}/runs", json_body=body, ok=(202,)).get(
            "run_id"
        )

    def get_run(self, wf_id: str, run_id: str) -> dict:
        return self._json("GET", f"/workflows/{wf_id}/runs/{run_id}")

    def get_step(self, wf_id: str, run_id: str, step_path: str) -> dict:
        return self._json(
            "GET", f"/workflows/{wf_id}/runs/{run_id}/steps", params={"path": step_path}
        )

    def cancel_run(self, wf_id: str, run_id: str) -> dict:
        return self._json("POST", f"/workflows/{wf_id}/runs/{run_id}/cancel")

    def wait_run(
        self, wf_id: str, run_id: str, *, timeout: float = 180.0, poll: float = 1.0
    ) -> dict:
        deadline = time.monotonic() + timeout
        while True:
            run = self.get_run(wf_id, run_id)
            if run.get("status") in TERMINAL_RUN:
                return run
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"run {run_id} が {timeout:.0f} 秒で終わりません: {run.get('status')}"
                )
            time.sleep(poll)

    # --- チャット ---

    def create_thread(self, title: str | None = None) -> dict:
        return self._json("POST", "/threads", json_body={"title": title} if title else {})

    def threads(self) -> list[dict]:
        """本人のスレッド全部（`next_cursor` の続きを読む）。"""
        items: list[dict] = []
        params: dict = {"limit": 100}
        while True:
            page = self._json("GET", "/threads", params=params)
            items += page.get("threads", [])
            if not page.get("next_cursor"):
                return items
            params = {"limit": 100, "cursor": page["next_cursor"]}

    def post_message(
        self,
        thread_id: str,
        text: str,
        *,
        model: str | None = None,
        effort: str | None = None,
        tools: list[str] | None = None,
    ) -> dict:
        body: dict = {"text": text}
        if model is not None:
            body["model"] = model
        if effort is not None:
            body["effort"] = effort
        if tools is not None:
            body["tools"] = tools
        self._guard(body)
        return self._json("POST", f"/threads/{thread_id}/messages", json_body=body, ok=(202,))

    def messages(self, thread_id: str) -> dict:
        return self._json("GET", f"/threads/{thread_id}/messages")

    def stream(
        self,
        thread_id: str,
        last_event_id: int = 0,
        *,
        timeout: float = 360.0,
        keepalive: bool = False,
    ) -> Iterator[dict]:
        """SSE（最も古い未完了の run）。終わりのイベントでサーバーが閉じる。run が無ければすぐ空で終わる。

        `keepalive=True` なら keep-alive のコメントも疑似のイベントとして返す（`iter_sse`）。
        """
        deadline = time.monotonic() + timeout
        if not self.logged_in:
            self.login()
        for attempt in (0, 1):
            with self._http.stream(
                "GET",
                f"/threads/{thread_id}/stream",
                params={"last_event_id": last_event_id},
                timeout=httpx.Timeout(self._timeout, read=60.0),
            ) as r:
                if r.status_code == 401 and attempt == 0:
                    r.read()
                    self.login()
                    continue
                if r.status_code != 200:
                    r.read()
                    raise ShikiError("GET", f"/threads/{thread_id}/stream", r.status_code, r.text)
                for ev in iter_sse(r.iter_lines(), keepalive=keepalive):
                    yield ev
                    if time.monotonic() >= deadline:
                        return
                return

    def approve(
        self, thread_id: str, run_id: str, *, tool_call_id: str, tool_name: str, approved: bool
    ) -> None:
        self.request(
            "POST",
            f"/threads/{thread_id}/runs/{run_id}/approvals",
            json_body={"tool_call_id": tool_call_id, "tool_name": tool_name, "approved": approved},
            ok=(204,),
        )

    def cancel_chat_run(self, thread_id: str, run_id: str) -> None:
        self.request("POST", f"/threads/{thread_id}/runs/{run_id}/cancel", ok=(204,))

    def delete_thread(self, thread_id: str, *, missing_ok: bool = False) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        return self.request("DELETE", f"/threads/{thread_id}", ok=ok).status_code == 204

    def wait_chat_idle(self, thread_id: str, *, timeout: float = 360.0, poll: float = 2.0) -> dict:
        """active_run_id が無くなるまで読み直す（2 秒ごと）。最後のメッセージ一覧を返す。"""
        deadline = time.monotonic() + timeout
        while True:
            page = self.messages(thread_id)
            if not page.get("active_run_id") and not page.get("queued_runs"):
                return page
            if time.monotonic() >= deadline:
                raise TimeoutError(f"thread {thread_id} の run が {timeout:.0f} 秒で終わりません")
            time.sleep(poll)

    # --- データ・ファイル ---

    def tables(self, q: str | None = None) -> list[dict]:
        items: list[dict] = []
        params: dict = {"limit": 200}
        if q:
            params["q"] = q
        while True:
            page = self._json("GET", "/data/tables", params=params)
            items += page.get("items", [])
            if not page.get("next_cursor"):
                return items
            params = {**params, "cursor": page["next_cursor"]}

    def get_table(self, table_id: str) -> dict:
        return self._json("GET", f"/data/tables/{table_id}")

    def create_table(self, name: str, schema: Mapping) -> dict:
        return self._json(
            "POST", "/data/tables", json_body={"name": name, "schema": schema}, ok=(201,)
        )

    def delete_table(self, table_id: str, *, missing_ok: bool = False) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        return self.request("DELETE", f"/data/tables/{table_id}", ok=ok).status_code == 204

    def records(self, table_id: str) -> list[dict]:
        items: list[dict] = []
        params: dict = {"limit": 200}
        while True:
            page = self._json("GET", f"/data/tables/{table_id}/records", params=params)
            items += page.get("items", [])
            if not page.get("next_cursor"):
                return items
            params = {"limit": 200, "cursor": page["next_cursor"]}

    def create_record(self, table_id: str, data: Mapping) -> dict:
        return self._json(
            "POST", f"/data/tables/{table_id}/records", json_body={"data": data}, ok=(201,)
        )

    def get_record(self, table_id: str, record_id: str) -> dict:
        return self._json("GET", f"/data/tables/{table_id}/records/{record_id}")

    def delete_record(
        self, table_id: str, record_id: str, expected_rev: int, *, missing_ok: bool = False
    ) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        r = self.request(
            "DELETE",
            f"/data/tables/{table_id}/records/{record_id}",
            params={"expected_rev": expected_rev},
            ok=ok,
        )
        return r.status_code == 204

    def create_folder(self, name: str, parent_id: str | None = None) -> dict:
        body: dict = {"name": name}
        if parent_id:
            body["parent_id"] = parent_id
        return self._json("POST", "/folders", json_body=body)

    def delete_folder(self, folder_id: str, *, missing_ok: bool = False) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        return self.request("DELETE", f"/folders/{folder_id}", ok=ok).status_code == 204

    def nodes(self, parent_id: str | None = None) -> list[dict]:
        items: list[dict] = []
        params: dict = {"limit": 100}
        if parent_id:
            params["parent_id"] = parent_id
        while True:
            page = self._json("GET", "/nodes", params=params)
            items += page.get("items", [])
            if not page.get("next_cursor"):
                return items
            params = {**params, "cursor": page["next_cursor"]}

    def node_by_name(self, name: str, parent_id: str | None = None) -> dict | None:
        params = {"name": name}
        if parent_id:
            params["parent_id"] = parent_id
        return self._json("GET", "/nodes/by-name", params=params).get("node")

    def tabular_save(self, name: str, csv: str, parent_id: str | None = None) -> dict:
        body: dict = {"name": name, "csv": csv}
        if parent_id:
            body["parent_id"] = parent_id
        return self._json("POST", "/tabular/save", json_body=body)

    def file_text(self, file_id: str) -> dict:
        return self._json("GET", f"/files/{file_id}/text")

    def delete_file(self, file_id: str, *, missing_ok: bool = False) -> bool:
        ok = (204, 404) if missing_ok else (204,)
        return self.request("DELETE", f"/files/{file_id}", ok=ok).status_code == 204


class _KeycloakJar:
    """Keycloak の Cookie（ログインの往復の間だけ持つ）。

    Keycloak は http でも `Secure` の Cookie（AUTH_SESSION_ID・KC_RESTART）を出す。ブラウザーと curl は localhost を
    安全な場所として扱って送り返すが、Python の cookiejar は https でなければ送らない（ログインの POST が
    「Cookie が見つかりません」の 400 になる）。そこで、authorize と同じ宛先にだけ、受けた Cookie を自分で付ける。
    """

    def __init__(self, authorize: str):
        u = httpx.URL(authorize)
        self.origin = (u.scheme, u.host, u.port)
        self.values: dict[str, str] = {}

    def send(self, client: httpx.Client, method: str, url, **kw) -> httpx.Response:
        u = httpx.URL(str(url))
        headers = {}
        if (u.scheme, u.host, u.port) == self.origin and self.values:
            headers["cookie"] = "; ".join(f"{k}={v}" for k, v in self.values.items())
        client.cookies.clear()
        r = client.request(method, u, headers=headers, **kw)
        if (u.scheme, u.host, u.port) == self.origin:
            for raw in r.headers.get_list("set-cookie"):
                pair = raw.split(";", 1)[0]
                name, sep, value = pair.partition("=")
                attrs = raw.lower()
                if not sep or not name.strip():
                    continue
                if "max-age=0" in attrs.replace(" ", "") or value == "":
                    self.values.pop(name.strip(), None)
                else:
                    self.values[name.strip()] = value.strip()
        return r


def capabilities_of(doc: Mapping, captured_tools: list[str] | None = None) -> dict:
    """OpenAPI の文書から版の機能の有無を読む（無い項目は False）。"""
    schemas = (doc.get("components") or {}).get("schemas") or {}
    paths = doc.get("paths") or {}

    def props(name: str) -> set[str]:
        return set(((schemas.get(name) or {}).get("properties") or {}).keys())

    start = props("StartRunRequest")
    return {
        "node_ports": "node_ports" in props("ValidateWorkflowResponse"),
        "trigger": "trigger" in start,
        "scheduled_at": "scheduled_at" in start,
        "custom_instructions": "/me/custom-instructions" in paths,
        # チャットの道具の一覧は OpenAPI に載らない（取り込んだ tools で決める・無ければ None）。
        "list_workflows": None if captured_tools is None else "list_workflows" in captured_tools,
    }


def _snippet(text: str, n: int = 300) -> str:
    """画面の一部（タグを落として詰めたもの）。"""
    plain = re.sub(r"<script.*?</script>|<style.*?</style>", " ", text, flags=re.S | re.I)
    plain = re.sub(r"<[^>]+>", " ", plain)
    plain = re.sub(r"\s+", " ", html.unescape(plain)).strip()
    return plain[:n]
