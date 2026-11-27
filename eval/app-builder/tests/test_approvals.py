"""abeval.approvals（既定で却下・許可リストだけ承認・tool_name を付ける・同じ要求は 1 回・S9 の網）。"""

from __future__ import annotations

from abeval import fixtures
from abeval.approvals import (
    ApprovalLog,
    ApprovalPolicy,
    handle_request,
    unapproved_side_effects,
)
from abeval.config import load_config
from abeval.shiki import ShikiClient
from fake_chat import ChatFake, RunScript

REQ = {
    "type": "approval_requested",
    "tool_call_id": "tc1",
    "name": "data_record_write",
    "input": {"table": "経費申請", "op": "create", "data": {"title": "x"}},
    "reason": "書き込み",
}


def test_default_policy_denies_everything_and_the_config_list_is_empty():
    cfg = load_config()
    assert cfg.mode_b.approve_tools == ()  # D-162: 許可リストは空
    p = ApprovalPolicy(frozenset(cfg.mode_b.approve_tools))
    assert not p.decide("data_record_write") and not p.decide("emit_workflow")
    assert ApprovalPolicy(frozenset({"try_workflow"})).decide("try_workflow")


def _thread(fake: ChatFake) -> tuple[ShikiClient, str, str]:
    c = ShikiClient("http://localhost:8080", "alice", transport=fake.transport())
    fixtures.init_sentinel(c)
    fake.scripts.append(RunScript(events=[REQ], hang=True))
    th = c.create_thread()["id"]
    run_id = c.post_message(th, "calltool:data_record_write {}", model="default")["run_id"]
    return c, th, run_id


def test_denies_with_the_tool_name_and_records_s9():
    fake = ChatFake()
    c, th, run_id = _thread(fake)
    log = ApprovalLog()
    entry = handle_request(c, th, run_id, REQ, ApprovalPolicy(), log)
    assert fake.approvals == [
        {
            "run_id": run_id,
            "tool_call_id": "tc1",
            "tool_name": "data_record_write",
            "approved": False,
        }
    ]
    assert entry["approved"] is False and entry["input_keys"] == ["data", "op", "table"]
    # 同じ要求が読み直しで重ねて届いても 1 回だけ。
    assert handle_request(c, th, run_id, REQ, ApprovalPolicy(), log) is None
    assert len(fake.approvals) == 1
    assert log.summary() == {
        "requests": 1,
        "approved": 0,
        "denied": 1,
        "tools": ["data_record_write"],
        "errors": [],
    }


def test_listed_tools_are_approved():
    fake = ChatFake()
    c, th, run_id = _thread(fake)
    log = ApprovalLog()
    handle_request(c, th, run_id, REQ, ApprovalPolicy(frozenset({"data_record_write"})), log)
    assert fake.approvals[0]["approved"] is True and log.summary()["approved"] == 1


def test_server_rejection_is_recorded_with_the_decision():
    fake = ChatFake()
    c, th, run_id = _thread(fake)
    log = ApprovalLog()
    wrong = {**REQ, "name": "other_tool"}  # 承認の要求と名前が違う → 400
    entry = handle_request(c, th, run_id, wrong, ApprovalPolicy(), log)
    assert entry["error"].startswith("400") and fake.approvals == []
    assert log.summary()["errors"]


def test_malformed_requests_are_ignored():
    fake = ChatFake()
    c, th, run_id = _thread(fake)
    assert (
        handle_request(
            c, th, run_id, {"type": "approval_requested"}, ApprovalPolicy(), ApprovalLog()
        )
        is None
    )


def test_side_effect_without_an_approval_request_is_serious():
    log = ApprovalLog()
    calls = [
        {"id": "a", "name": "data_record_write"},
        {"id": "b", "name": "data_record_write"},
        {"id": "c", "name": "data_query"},
    ]
    results = {"a": {"ok": True}, "b": {"ok": True}, "c": {"ok": True}}
    log.add({"tool_call_id": "b", "name": "data_record_write"}, "r", False)
    assert unapproved_side_effects(calls, results, log) == [
        {"tool_call_id": "a", "tool_name": "data_record_write"}
    ]
    assert unapproved_side_effects(calls, {"a": {"ok": False}}, log) == []
