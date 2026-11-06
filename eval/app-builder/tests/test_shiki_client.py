"""abeval.shiki（OIDC のログインの連鎖・CSRF・401 の再ログイン・stub の守り・SSE・機能の判定）。"""

from __future__ import annotations

import httpx
import pytest

from abeval.shiki import (
    LoginError,
    ShikiClient,
    ShikiError,
    StubOnlyViolation,
    assert_stub_only,
    capabilities_of,
    iter_sse,
)
from fake_shiki import FakeShiki

API = "http://localhost:8080"


@pytest.fixture
def fake():
    return FakeShiki()


def client(fake, user="alice", **kw) -> ShikiClient:
    return ShikiClient(API, user, transport=fake.transport(), **kw)


def test_oidc_chain_and_callback_host_rewrite(fake):
    with client(fake) as c:
        me = c.login()
    assert me["org"] == "a-corp"
    seq = [(r.method, r.url.host, r.url.port, r.url.path) for r in fake.seen]
    assert seq == [
        ("GET", "localhost", 8080, "/auth/login"),
        ("GET", "localhost", 8081, "/realms/shiki/protocol/openid-connect/auth"),
        ("POST", "localhost", 8081, "/realms/shiki/login-actions/authenticate"),
        # 戻り先（:3000）の scheme・host・port を API に置き換え、path と query はそのまま。
        ("GET", "localhost", 8080, "/auth/callback"),
        ("GET", "localhost", 8080, "/me"),
    ]
    cb = fake.seen[3]
    assert cb.url.params["code"].startswith("code-alice-")
    assert cb.url.params["state"]
    # kc-form-login の action（passkey の form ではない）を &amp; を戻して使った。
    post = fake.seen[2]
    assert post.url.params["execution"] == "ex1" and post.url.params["tab_id"] == "t1"
    assert b"username=alice" in post.content
    assert c.logins == 1


def test_callback_without_rewrite_would_not_reach_the_api(fake):
    # API の基底が :3000 のときは書き換えても :3000 のまま（偽の shiki は届かない宛先として 502）。
    c = ShikiClient("http://localhost:3000", "alice", transport=fake.transport())
    with c, pytest.raises(LoginError):
        c.login()


def test_users_have_separate_cookie_jars(fake):
    a, ch = client(fake, "alice"), client(fake, "charlie")
    assert a.login()["org"] == "a-corp"
    assert ch.login()["org"] == "b-corp"
    assert a.me()["org"] == "a-corp"
    assert ch.me()["org"] == "b-corp"


def test_bad_password_shows_a_snippet(fake):
    with client(fake, password="wrong") as c, pytest.raises(LoginError) as e:
        c.login()
    assert "Invalid username or password." in str(e.value)
    assert "wrong" not in str(e.value)


def test_missing_form_action_shows_a_snippet():
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/auth/login":
            return httpx.Response(302, headers={"location": "http://localhost:8081/x"})
        return httpx.Response(200, text="<html><h1>Keycloak の画面が変わった</h1></html>")

    c = ShikiClient(API, "alice", transport=httpx.MockTransport(handler))
    with pytest.raises(LoginError, match="form action を取れません.*画面が変わった"):
        c.login()


def test_csrf_header_only_on_state_changing_requests(fake):
    with client(fake) as c:
        c.login()
        c.me()
        c.llm_models(purpose="workflow")
        folder = c.create_folder("x")
        c.delete_folder(folder["id"])
        t = c.create_table("t", {"fields": []})
        c.delete_table(t["id"])
        c.update_workflow(
            c.create_workflow({"name": "w", "nodes": []})["id"], {"name": "w", "nodes": []}
        )
    api = [r for r in fake.seen if r.url.port == 8080 and not r.url.path.startswith("/auth/")]
    for r in api:
        has = "x-csrf-token" in r.headers
        if r.method in ("POST", "PUT", "PATCH", "DELETE"):
            assert has and r.headers["x-csrf-token"], (r.method, r.url.path)
        else:
            assert not has, (r.method, r.url.path)
    assert {r.method for r in api} >= {"GET", "POST", "PUT", "DELETE"}
    # Keycloak とログインの往復にも付けない。
    assert not any("x-csrf-token" in r.headers for r in fake.seen if r.url.port == 8081)


def test_wrong_csrf_is_rejected_by_the_fake(fake):
    # 偽の shiki が CSRF を本当に照合していること（上のテストの前提）。
    with client(fake) as c:
        c.login()
        r = c._http.post("/folders", json={"name": "x"}, headers={"x-csrf-token": "nope"})
        assert r.status_code == 403


def test_401_relogs_in_once(fake):
    with client(fake) as c:
        c.login()
        fake.expire("alice")
        assert c.me()["org"] == "a-corp"
        assert c.logins == 2
        assert len(fake.requests_to("/auth/callback")) == 2


def test_401_after_relogin_is_an_error(fake):
    """ログインし直しても 401 なら、もう一度は繰り返さずに誤りにする。"""

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/data/tables":
            fake.seen.append(request)
            return httpx.Response(401, json={"title": "未認証"})
        return fake.handle(request)

    c = ShikiClient(API, "alice", transport=httpx.MockTransport(handler))
    c.login()
    with pytest.raises(ShikiError) as e:
        c.tables()
    assert e.value.status == 401
    assert c.logins == 2
    assert len(fake.requests_to("/data/tables")) == 2


def test_401_on_me_right_after_login_does_not_loop(fake):
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/me":
            return httpx.Response(401, json={"title": "未認証"})
        return fake.handle(request)

    c = ShikiClient(API, "alice", transport=httpx.MockTransport(handler))
    with pytest.raises(ShikiError):
        c.me()
    assert c.logins == 1


def test_lazy_login_and_errors(fake):
    with client(fake) as c:
        assert c.me()["org"] == "a-corp"  # 初回はログインしてから送る
        with pytest.raises(ShikiError) as e:
            c.get_workflow("00000000-0000-0000-0000-000000000000")
        assert e.value.status == 404
        assert c.delete_artifact("00000000-0000-0000-0000-000000000000", missing_ok=True) is False


def test_custom_instructions_404_is_none(fake):
    fake.custom_instructions = None
    with client(fake) as c:
        assert c.custom_instructions() is None


def test_capabilities_from_openapi(fake):
    with client(fake) as c:
        caps = c.capabilities()
    assert caps == {
        "node_ports": True,
        "trigger": True,
        "scheduled_at": True,
        "custom_instructions": True,
        "list_workflows": None,
    }
    # V0 のように項目の無い版。
    v0 = {
        "paths": {"/workflows": {}},
        "components": {
            "schemas": {
                "ValidateWorkflowResponse": {"properties": {"errors": {}}},
                "StartRunRequest": {"properties": {"input": {}}},
            }
        },
    }
    assert capabilities_of(v0, ["emit_workflow", "read_workflow"]) == {
        "node_ports": False,
        "trigger": False,
        "scheduled_at": False,
        "custom_instructions": False,
        "list_workflows": False,
    }
    # openapi は認証なしで読む（ログインしない）。
    assert not any(r.url.path == "/auth/login" for r in fake.seen)


def test_start_run_body(fake):
    with client(fake) as c:
        wid = c.create_workflow({"name": "w", "nodes": []})["id"]
        rid = c.start_run(wid, {"a": 1}, scheduled_at="2026-11-09T09:00")
        assert c.wait_run(wid, rid)["status"] == "succeeded"
    assert fake.runs[rid]["body"] == {"input": {"a": 1}, "scheduled_at": "2026-11-09T09:00"}


# --- stub の守り ---

LLM_IR = {
    "name": "ok-flow",
    "nodes": [{"id": "a", "type": "llm.invoke", "params": {"model": "default", "prompt": "p"}}],
}


def test_stub_guard_accepts_stub_models():
    assert_stub_only(LLM_IR)
    assert_stub_only(
        {"nodes": [{"id": "d", "type": "ai.decide", "params": {"model": "decide-stub"}}]}
    )
    assert_stub_only({"nodes": [{"id": "s", "type": "script.run", "params": {}}]})
    assert_stub_only({"text": "こんにちは", "model": "default"})


@pytest.mark.parametrize(
    "obj",
    [
        {"nodes": [{"id": "a", "type": "llm.invoke", "params": {"model": "claude-haiku-5-5"}}]},
        {"nodes": [{"id": "a", "type": "llm.invoke", "params": {"prompt": "p"}}]},  # 省略
        {"nodes": [{"id": "d", "type": "ai.decide", "params": {"model": "decide-haiku"}}]},
        {
            "nodes": [
                {"id": "a", "type": "x", "params": {"prompt": {"$template": "claude-haiku-5-5"}}}
            ]
        },
        {"text": "こんにちは"},  # model を明示しない
        {"text": "こんにちは", "model": "claude-haiku-5-5"},
        {
            "text": 'calltool:emit_workflow {"ir":{"nodes":[{"params":{"model":"claude-haiku-5-5"}}]}}',
            "model": "default",
        },
        {"name": "neither"},
    ],
)
def test_stub_guard_rejects_real_models(obj):
    with pytest.raises(StubOnlyViolation):
        assert_stub_only(obj)


def test_stub_only_client_blocks_before_sending(fake):
    with client(fake, stub_only=True) as c:
        c.login()
        before = len(fake.seen)
        bad_ir = {
            "name": "x",
            "nodes": [{"id": "a", "type": "llm.invoke", "params": {"model": "claude-haiku-5-5"}}],
        }
        with pytest.raises(StubOnlyViolation):
            c.create_workflow(bad_ir)
        th = c.create_thread()
        with pytest.raises(StubOnlyViolation):
            c.post_message(th["id"], "やあ", model="claude-haiku-5-5")
        assert [r.url.path for r in fake.seen[before:]] == ["/threads"]
        # 保存済みのワークフローを動かす前にも IR を確かめる（stub_only でない版で保存されたもの）。
        wid = client(fake).create_workflow(bad_ir)["id"]
        with pytest.raises(StubOnlyViolation):
            c.start_run(wid, {})
        assert not fake.runs
        ok = c.create_workflow(LLM_IR)
        assert c.start_run(ok["id"], {})
        c.post_message(th["id"], "やあ", model="default")


# --- SSE ---


def test_iter_sse():
    lines = [
        ": keep-alive",
        "id: 1",
        'data: {"type":"token","text":"a"}',
        "",
        "id: 2",
        'data: {"type":"approval_requested",',
        'data: "tool_call_id":"t1","name":"data_record_write"}',
        "",
        "data: not json",
        "",
        "id: 3\r",
        'data: {"type":"done","message_id":"m"}',
    ]
    evs = list(iter_sse(iter(lines)))
    assert evs[0] == {"id": 1, "data": {"type": "token", "text": "a"}}
    assert evs[1]["data"]["name"] == "data_record_write" and evs[1]["id"] == 2
    assert evs[2]["data"]["type"] == "_unparsed"
    assert evs[3] == {"id": 3, "data": {"type": "done", "message_id": "m"}}


def test_stream_reads_events_and_approve(fake):
    with client(fake) as c:
        th = c.create_thread()
        fake.sse[th["id"]] = (
            'id: 1\ndata: {"type":"approval_requested","tool_call_id":"t1","name":"data_record_write"}\n\n'
            'id: 2\ndata: {"type":"done","message_id":"m"}\n\n'
        )
        evs = list(c.stream(th["id"]))
        assert [e["data"]["type"] for e in evs] == ["approval_requested", "done"]
        c.approve(th["id"], "r1", tool_call_id="t1", tool_name="data_record_write", approved=False)
        c.cancel_chat_run(th["id"], "r1")
    stream_req = fake.requests_to(f"/threads/{th['id']}/stream")[0]
    assert stream_req.url.params["last_event_id"] == "0"
    assert "x-csrf-token" not in stream_req.headers
    assert fake.threads[th["id"]]["approvals"] == [
        {"tool_call_id": "t1", "tool_name": "data_record_write", "approved": False}
    ]


def test_post_message_body(fake):
    with client(fake) as c:
        th = c.create_thread()
        out = c.post_message(th["id"], "やあ", model="default", effort="low")
        assert out["run_id"]
        assert fake.threads[th["id"]]["posted"] == {
            "text": "やあ",
            "model": "default",
            "effort": "low",
        }
        page = c.wait_chat_idle(th["id"])
        assert page["messages"][-1]["content"][0]["text"] == "回答: やあ"
        assert c.delete_thread(th["id"])


def test_usage_params(fake):
    with client(fake) as c:
        out = c.usage(from_="2026-11-01", to="2026-11-06", source="chat")
    assert out["params"] == {"from": "2026-11-01", "to": "2026-11-06", "source": "chat"}


def test_bad_api_url():
    with pytest.raises(ValueError):
        ShikiClient("localhost:8080", "alice")


def test_threads_follow_the_cursor(fake):
    with client(fake) as c:
        made = {c.create_thread()["id"] for _ in range(5)}
        assert {t["id"] for t in c.threads()} == made
    with client(fake, "charlie") as ch:
        assert ch.threads() == []
