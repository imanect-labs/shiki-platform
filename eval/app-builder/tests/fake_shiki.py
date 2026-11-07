"""偽の shiki と Keycloak（httpx.MockTransport の手続き・ソケットを使わない）。

- OIDC: `/auth/login` は相関 Cookie を出して Keycloak の authorize（:8081）へ 302。Keycloak の画面は passkey の form
  （別の action）と `kc-form-login` を返す（action の `&amp;` を戻さないと session_code の照合で落ちる）。資格情報の
  POST は `redirect_uri`（既定は dev-up と同じ :3000）へ code つきで 302。`/auth/callback` は API のホスト（:8080）で
  受けたときだけ相関 Cookie と state を照合してセッションを出す（:3000 に来たら 502 にする）。
- 状態を変える要求は `x-csrf-token` が Cookie `shiki_csrf` とセッションの値に一致しなければ 403。
- 資源はテナントごと（charlie の b-corp から a-corp のものは 404）。受けた要求は `seen` に残す。
- 状態の進め方（`POST /data/fsms`・スキーマの改訂の `fsm_ref`・`…/transition`）は製品と同じく、行を足したときの
  状態を最初の状態に決め、定義に無い進め方は 400 にする。定義はテーブルを消しても残る（`DELETE /artifacts` で消える）。
"""

from __future__ import annotations

import datetime as dt
import json
import secrets
import uuid
from dataclasses import dataclass, field
from urllib.parse import parse_qs, urlencode

import httpx

API_HOST = ("localhost", 8080)
KC = "http://localhost:8081/realms/shiki"
USERS = {"alice": "a-corp", "bob": "a-corp", "charlie": "b-corp"}
PASSWORD = "password"


def _now() -> str:
    return dt.datetime.now(dt.UTC).isoformat()


def _cookies(request: httpx.Request) -> dict[str, str]:
    out = {}
    for part in request.headers.get("cookie", "").split(";"):
        k, _, v = part.strip().partition("=")
        if k:
            out[k] = v
    return out


def _json(status: int, body: object, headers: list | None = None) -> httpx.Response:
    return httpx.Response(status, json=body, headers=headers or [])


@dataclass
class Session:
    user: str
    csrf: str


@dataclass
class FakeShiki:
    redirect_uri: str = "http://localhost:3000/auth/callback"
    api_port: int = API_HOST[1]
    custom_instructions: dict | None = field(
        default_factory=lambda: {"about": "", "style": "", "tone": "default"}
    )
    models: list[str] = field(default_factory=lambda: ["default", "claude-haiku-5-5"])
    openapi_doc: dict = field(
        default_factory=lambda: {
            "paths": {
                "/me/custom-instructions": {},
                "/workflows": {},
                "/data/fsms": {},
                "/data/tables/{id}/schema": {},
                "/data/tables/{id}/records/{record_id}/transition": {},
            },
            "components": {
                "schemas": {
                    "ValidateWorkflowResponse": {
                        "properties": {"errors": {}, "warnings": {}, "node_ports": {}}
                    },
                    "StartRunRequest": {
                        "properties": {"input": {}, "trigger": {}, "scheduled_at": {}}
                    },
                }
            },
        }
    )
    seen: list[httpx.Request] = field(default_factory=list)
    sessions: dict[str, Session] = field(default_factory=dict)
    flows: dict[str, str] = field(default_factory=dict)
    codes: dict[str, str] = field(default_factory=dict)
    kc_sessions: set[str] = field(default_factory=set)
    # 資源（id → dict）。どれも tenant を持つ。
    nodes: dict[str, dict] = field(default_factory=dict)
    tables: dict[str, dict] = field(default_factory=dict)
    records: dict[str, dict] = field(default_factory=dict)
    workflows: dict[str, dict] = field(default_factory=dict)
    threads: dict[str, dict] = field(default_factory=dict)
    runs: dict[str, dict] = field(default_factory=dict)
    # 状態の進め方の定義（artifact(kind=fsm)・テーブルを消しても残る）。
    fsms: dict[str, dict] = field(default_factory=dict)
    transitions: list[tuple[str, str, str]] = field(default_factory=list)
    sse: dict[str, str] = field(default_factory=dict)
    fail_next: dict[str, int] = field(default_factory=dict)

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self.handle)

    def expire(self, user: str) -> None:
        """その利用者のセッションを切る（次の要求は 401）。"""
        for sid in [s for s, v in self.sessions.items() if v.user == user]:
            del self.sessions[sid]

    def requests_to(self, path_prefix: str, method: str | None = None) -> list[httpx.Request]:
        return [
            r
            for r in self.seen
            if r.url.path.startswith(path_prefix) and (method is None or r.method == method)
        ]

    # --- 振り分け ---

    def handle(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        host = (request.url.host, request.url.port)
        if host == ("localhost", 8081):
            return self._keycloak(request)
        if host != (API_HOST[0], self.api_port):
            return httpx.Response(502, text=f"届かない宛先: {request.url}")
        path = request.url.path
        if path == "/healthz":
            return httpx.Response(200, text="ok")
        if path == "/api-docs/openapi.json":
            return _json(200, self.openapi_doc)
        if path == "/auth/login":
            return self._login()
        if path == "/auth/callback":
            return self._callback(request)
        sess = self.sessions.get(_cookies(request).get("shiki_session", "").split(".")[0])
        if sess is None:
            return _json(401, {"title": "未認証"})
        if request.method in ("POST", "PUT", "PATCH", "DELETE"):
            header = request.headers.get("x-csrf-token")
            if not header or header != _cookies(request).get("shiki_csrf") or header != sess.csrf:
                return _json(403, {"title": "CSRF"})
        n = self.fail_next.get(path, 0)
        if n:
            self.fail_next[path] = n - 1
            return _json(500, {"title": "わざと"})
        return self._api(request, sess.user, USERS[sess.user])

    # --- OIDC ---

    def _login(self) -> httpx.Response:
        state = secrets.token_hex(8)
        flow = secrets.token_hex(8)
        self.flows[flow] = state
        q = urlencode(
            {
                "response_type": "code",
                "client_id": "shiki-web",
                "redirect_uri": self.redirect_uri,
                "state": state,
            }
        )
        return httpx.Response(
            302,
            headers=[
                ("location", f"{KC}/protocol/openid-connect/auth?{q}"),
                ("set-cookie", f"shiki_oidc_flow={flow}; Path=/; HttpOnly"),
            ],
        )

    def _keycloak(self, request: httpx.Request) -> httpx.Response:
        c = _cookies(request)
        if "shiki_session" in c or "shiki_csrf" in c or "shiki_oidc_flow" in c:
            return httpx.Response(400, text="アプリの Cookie が Keycloak に届いた")
        if request.url.path.endswith("/protocol/openid-connect/auth"):
            p = request.url.params
            kc = secrets.token_hex(6)
            self.kc_sessions.add(kc)
            sc = f"sc{kc}"
            q = f"session_code={sc}&amp;execution=ex1&amp;client_id=shiki-web&amp;tab_id=t1"
            ru, st = p["redirect_uri"], p["state"]
            self.codes[sc] = json.dumps({"ru": ru, "st": st})
            page = (
                "<html><body>"
                f'<form id="kc-passkey-form" action="{KC}/login-actions/passkey?x=1" method="post"></form>'
                f'<form id="kc-form-login" class="x" onsubmit="return true;" action="{KC}/login-actions/authenticate?{q}" method="post">'
                '<input id="username" name="username"/><input id="password" name="password"/></form>'
                "</body></html>"
            )
            return httpx.Response(
                200,
                text=page,
                # 本物の Keycloak と同じく、http でも Secure を付ける。
                headers=[
                    (
                        "set-cookie",
                        f"AUTH_SESSION_ID={kc};Version=1;Path=/realms/shiki/;Secure;HttpOnly;SameSite=None",
                    ),
                    (
                        "set-cookie",
                        "KC_RESTART=r;Version=1;Path=/realms/shiki/;Secure;HttpOnly;SameSite=None",
                    ),
                ],
            )
        if request.url.path.endswith("/login-actions/authenticate"):
            p = request.url.params
            if p.get("execution") != "ex1" or p.get("client_id") != "shiki-web":
                return httpx.Response(400, text="action の &amp; を戻していない")
            sc = p.get("session_code", "")
            if sc not in self.codes or c.get("AUTH_SESSION_ID") != sc[2:]:
                return httpx.Response(400, text="Keycloak のセッションが合わない")
            form = parse_qs(request.content.decode())
            user = (form.get("username") or [""])[0]
            if user not in USERS or (form.get("password") or [""])[0] != PASSWORD:
                return httpx.Response(
                    200,
                    text="<html><span id='input-error'>Invalid username or password.</span></html>",
                )
            info = json.loads(self.codes.pop(sc))
            code = f"code-{user}-{secrets.token_hex(4)}"
            self.codes[code] = user
            return httpx.Response(
                302, headers=[("location", f"{info['ru']}?state={info['st']}&code={code}")]
            )
        return httpx.Response(404, text="keycloak")

    def _callback(self, request: httpx.Request) -> httpx.Response:
        p = request.url.params
        flow = _cookies(request).get("shiki_oidc_flow")
        if flow not in self.flows or self.flows[flow] != p.get("state"):
            return _json(401, {"title": "state"})
        user = self.codes.pop(p.get("code", ""), None)
        if user not in USERS:
            return _json(401, {"title": "code"})
        del self.flows[flow]
        sid, csrf = secrets.token_hex(8), secrets.token_hex(8)
        self.sessions[sid] = Session(user, csrf)
        return httpx.Response(
            303,
            headers=[
                ("location", "/"),
                ("set-cookie", f"shiki_session={sid}.{USERS[user]}; Path=/; HttpOnly"),
                ("set-cookie", f"shiki_csrf={csrf}; Path=/"),
                ("set-cookie", "shiki_oidc_flow=; Path=/; Max-Age=0"),
            ],
        )

    # --- API ---

    def _api(self, request: httpx.Request, user: str, tenant: str) -> httpx.Response:
        m, path = request.method, request.url.path
        parts = path.strip("/").split("/")
        body = json.loads(request.content) if request.content else {}
        params = request.url.params

        def mine(store: dict, id_: str) -> dict | None:
            v = store.get(id_)
            return v if v and v["tenant"] == tenant else None

        if path == "/me" and m == "GET":
            return _json(200, {"id": user, "org": tenant, "tenant_id": tenant, "roles": []})
        if path == "/me/custom-instructions":
            if self.custom_instructions is None:
                return _json(404, {"title": "not found"})
            return _json(200, self.custom_instructions)
        if path == "/llm/models":
            return _json(
                200, {"default_model": "default", "models": [{"id": i} for i in self.models]}
            )
        if path == "/org-admin/ai/usage":
            return _json(
                200, {"totals": {"cost_usd_micros": 0, "calls": 0}, "params": dict(params)}
            )
        if path == "/workflows/validate" and m == "POST":
            return _json(
                200, {"errors": [], "warnings": [], "declared_scopes": [], "node_ports": {}}
            )
        if path == "/workflows" and m == "POST":
            ir = body["ir"]
            if any(
                w["name"] == ir["name"] for w in self.workflows.values() if w["tenant"] == tenant
            ):
                return _json(409, {"code": "name_taken"})
            wid = str(uuid.uuid4())
            self.workflows[wid] = {
                "id": wid,
                "tenant": tenant,
                "name": ir["name"],
                "ir": ir,
                "version": 1,
                "updated_at": _now(),
            }
            return _json(201, {"id": wid, "version": 1, "name": ir["name"], "warnings": []})
        if path == "/workflows" and m == "GET":
            items = [
                {"id": w["id"], "name": w["name"], "updated_at": w["updated_at"]}
                for w in self.workflows.values()
                if w["tenant"] == tenant
            ]
            return _json(200, {"items": items, "owned_next": None, "shared_next_cursor": None})
        if parts[0] == "workflows" and len(parts) >= 2:
            w = mine(self.workflows, parts[1])
            if w is None:
                return _json(404, {"title": "not found"})
            if len(parts) == 2 and m == "GET":
                return _json(200, {"id": w["id"], "version": w["version"], "ir": w["ir"]})
            if len(parts) == 2 and m == "PUT":
                w["ir"], w["version"] = body["ir"], w["version"] + 1
                return _json(200, {"id": w["id"], "version": w["version"]})
            if len(parts) == 3 and parts[2] == "runs" and m == "POST":
                rid = str(uuid.uuid4())
                self.runs[rid] = {
                    "run_id": rid,
                    "tenant": tenant,
                    "status": "succeeded",
                    "body": body,
                    "steps": [],
                }
                return _json(202, {"run_id": rid})
            if len(parts) == 4 and parts[2] == "runs" and m == "GET":
                r = mine(self.runs, parts[3])
                return _json(200, r) if r else _json(404, {})
        if parts[0] == "artifacts" and len(parts) == 2 and m == "DELETE":
            for store in (self.workflows, self.fsms):
                if mine(store, parts[1]) is not None:
                    del store[parts[1]]
                    return httpx.Response(204)
            return _json(404, {})
        if parts[0:2] == ["data", "fsms"]:
            return self._fsm_api(m, parts, body, tenant)
        if path == "/folders" and m == "POST":
            return self._new_node(tenant, body["name"], body.get("parent_id"), "folder")
        if parts[0] == "folders" and len(parts) == 2 and m == "DELETE":
            n = mine(self.nodes, parts[1])
            if n is None:
                return _json(404, {})
            for k in [k for k, v in self.nodes.items() if v["parent_id"] == parts[1]]:
                del self.nodes[k]
            del self.nodes[parts[1]]
            return httpx.Response(204)
        if path == "/nodes":
            parent = params.get("parent_id")
            items = [
                self._node_view(n)
                for n in self.nodes.values()
                if n["tenant"] == tenant and n["parent_id"] == parent
            ]
            return _json(200, {"items": items, "next_cursor": None})
        if path == "/nodes/by-name":
            parent = params.get("parent_id")
            hit = [
                self._node_view(n)
                for n in self.nodes.values()
                if n["tenant"] == tenant
                and n["parent_id"] == parent
                and n["name"] == params["name"]
            ]
            return _json(200, {"node": hit[0] if hit else None})
        if path == "/tabular/save" and m == "POST":
            r = self._new_node(tenant, body["name"], body.get("parent_id"), "file", body["csv"])
            if r.status_code != 200:
                return r
            n = r.json()
            return _json(200, {"node_id": n["id"], "version": 1, "name": n["name"]})
        if parts[0] == "files" and len(parts) >= 2:
            n = mine(self.nodes, parts[1])
            if n is None or n["kind"] != "file":
                return _json(404, {})
            if m == "DELETE":
                del self.nodes[parts[1]]
                return httpx.Response(204)
            return _json(200, {"text": n["text"], "encoding": "utf-8"})
        if path == "/data/tables" and m == "POST":
            if any(
                t["name"] == body["name"] for t in self.tables.values() if t["tenant"] == tenant
            ):
                return _json(409, {"code": "name_taken"})
            tid = str(uuid.uuid4())
            self.tables[tid] = {
                "id": tid,
                "tenant": tenant,
                "name": body["name"],
                "schema": body["schema"],
                "schema_version": 1,
                "created_at": _now(),
            }
            return _json(201, self._pub(self.tables[tid]))
        if path == "/data/tables" and m == "GET":
            q = (params.get("q") or "").lower()
            items = [
                self._pub(t)
                for t in self.tables.values()
                if t["tenant"] == tenant and q in t["name"].lower()
            ]
            return _json(200, {"items": items, "next_cursor": None})
        if parts[0:2] == ["data", "tables"] and len(parts) >= 3:
            t = mine(self.tables, parts[2])
            if t is None:
                return _json(404, {})
            if len(parts) == 3 and m == "GET":
                return _json(200, self._pub(t))
            if len(parts) == 3 and m == "DELETE":
                for k in [k for k, v in self.records.items() if v["table_id"] == t["id"]]:
                    del self.records[k]
                del self.tables[t["id"]]
                return httpx.Response(204)
            if parts[3:] == ["schema"] and m == "PUT":
                if body.get("expected_schema_version") != t["schema_version"]:
                    return _json(409, {"code": "conflict"})
                t["schema"], t["schema_version"] = body["schema"], t["schema_version"] + 1
                return _json(200, self._pub(t))
            if len(parts) == 4 and m == "POST":
                data = dict(body["data"])
                fsm = self._pinned(t)
                if fsm is not None:
                    # 足したときの状態は最初の状態に決まる（data の fsm_rules）。
                    key, initial = t["schema"]["status_field"], fsm["states"][0]
                    if data.get(key) is None:
                        data[key] = initial
                    elif data[key] != initial:
                        return _json(400, {"code": "fsm_invalid"})
                rid = str(uuid.uuid4())
                self.records[rid] = {
                    "id": rid,
                    "tenant": tenant,
                    "table_id": t["id"],
                    "data": data,
                    "rev": 1,
                }
                return _json(201, self._pub(self.records[rid]))
            if len(parts) == 4 and m == "GET":
                items = [self._pub(r) for r in self.records.values() if r["table_id"] == t["id"]]
                return _json(200, {"items": items, "next_cursor": None})
            if len(parts) == 6 and parts[5] == "transition" and m == "POST":
                return self._transition(t, parts[4], body)
            if len(parts) == 5:
                r = self.records.get(parts[4])
                if r is None or r["table_id"] != t["id"]:
                    return _json(404, {})
                if m == "GET":
                    return _json(200, self._pub(r))
                if m == "DELETE":
                    if int(params.get("expected_rev", -1)) != r["rev"]:
                        return _json(409, {"code": "rev_conflict"})
                    del self.records[r["id"]]
                    return httpx.Response(204)
        if path == "/threads" and m == "GET":
            mine_ = [{"id": t["id"]} for t in self.threads.values() if t["tenant"] == tenant]
            start = int(params.get("cursor") or 0)
            page = mine_[start : start + 2]  # 続きを読むことを確かめるため 2 件ずつ
            nxt = str(start + 2) if start + 2 < len(mine_) else None
            return _json(200, {"threads": page, "next_cursor": nxt})
        if path == "/threads" and m == "POST":
            tid = str(uuid.uuid4())
            self.threads[tid] = {"id": tid, "tenant": tenant, "messages": [], "active": None}
            return _json(200, {"id": tid, "title": body.get("title")})
        if parts[0] == "threads" and len(parts) >= 2:
            th = mine(self.threads, parts[1])
            if th is None:
                return _json(404, {})
            if len(parts) == 2 and m == "DELETE":
                del self.threads[th["id"]]
                return httpx.Response(204)
            if parts[2:] == ["messages"] and m == "POST":
                rid = str(uuid.uuid4())
                th["messages"].append(
                    {"role": "user", "content": [{"type": "text", "text": body["text"]}]}
                )
                th["messages"].append(
                    {
                        "role": "assistant",
                        "content": [{"type": "text", "text": f"回答: {body['text']}"}],
                    }
                )
                th["posted"] = body
                return _json(
                    202, {"run_id": rid, "user_message_id": "u", "assistant_message_id": "a"}
                )
            if parts[2:] == ["messages"] and m == "GET":
                return _json(200, {"messages": th["messages"]})
            if parts[2:] == ["stream"]:
                return httpx.Response(
                    200,
                    text=self.sse.get(th["id"], ""),
                    headers=[("content-type", "text/event-stream")],
                )
            if len(parts) == 5 and parts[2] == "runs" and parts[4] in ("approvals", "cancel"):
                th.setdefault(parts[4], []).append(body)
                return httpx.Response(204)
        return _json(404, {"title": f"偽の shiki に無い口: {m} {path}"})

    def _fsm_api(self, m: str, parts: list[str], body: dict, tenant: str) -> httpx.Response:
        if len(parts) == 2 and m == "POST":
            t = self.tables.get(body.get("table_id", ""))
            if t is None or t["tenant"] != tenant:
                return _json(404, {})
            key = t["schema"].get("status_field")
            f = next((x for x in t["schema"]["fields"] if x["key"] == key), None)
            states = body["body"]["states"]
            if f is None or set(f.get("options") or []) != set(states):
                return _json(400, {"code": "fsm_invalid"})
            if any(x["name"] == body["name"] for x in self.fsms.values() if x["tenant"] == tenant):
                return _json(409, {"code": "name_taken"})
            fid = str(uuid.uuid4())
            self.fsms[fid] = {
                "id": fid,
                "tenant": tenant,
                "name": body["name"],
                "body": body["body"],
            }
            return _json(201, {"id": fid, "version": 1, "body": body["body"]})
        x = self.fsms.get(parts[2]) if len(parts) == 3 else None
        if x is None or x["tenant"] != tenant:
            return _json(404, {})
        return _json(200, {"id": x["id"], "version": 1, "body": x["body"]})

    def _pinned(self, t: dict) -> dict | None:
        """テーブルが指す状態の進め方の定義（無い・消えたら None）。"""
        r = t["schema"].get("fsm_ref")
        x = self.fsms.get(r["artifact_id"]) if r else None
        return x["body"] if x and t["schema"].get("status_field") else None

    def _transition(self, t: dict, record_id: str, body: dict) -> httpx.Response:
        r = self.records.get(record_id)
        if r is None or r["table_id"] != t["id"]:
            return _json(404, {})
        fsm = self._pinned(t)
        if fsm is None:
            return _json(400, {"code": "fsm_invalid"})
        if body.get("expected_rev") != r["rev"]:
            return _json(409, {"code": "rev_conflict", "current_rev": r["rev"]})
        key = t["schema"]["status_field"]
        if not any(
            x["from"] == r["data"].get(key) and x["to"] == body["to"] for x in fsm["transitions"]
        ):
            return _json(400, {"code": "fsm_invalid"})
        r["data"] = {**r["data"], key: body["to"]}
        r["rev"] += 1
        self.transitions.append((t["id"], record_id, body["to"]))
        return _json(200, self._pub(r))

    def _new_node(self, tenant, name, parent, kind, text=None) -> httpx.Response:
        if any(
            n["name"] == name and n["parent_id"] == parent and n["tenant"] == tenant
            for n in self.nodes.values()
        ):
            return _json(409, {"code": "name_taken"})
        nid = str(uuid.uuid4())
        self.nodes[nid] = {
            "id": nid,
            "tenant": tenant,
            "name": name,
            "parent_id": parent,
            "kind": kind,
            "text": text,
            "created_at": _now(),
        }
        return _json(200, self._node_view(self.nodes[nid]))

    @staticmethod
    def _node_view(n: dict) -> dict:
        return {k: n[k] for k in ("id", "name", "parent_id", "kind", "created_at")}

    @staticmethod
    def _pub(v: dict) -> dict:
        return {k: x for k, x in v.items() if k != "tenant"}
