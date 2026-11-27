"""Mode B の記録の読み取り（stub のスタックで記録した会話 `tests/fixtures/transcripts/*.json`）。

記録は `ABEVAL_RECORD_TRANSCRIPTS=tests/fixtures/transcripts ABEVAL_API_URL=… pytest -m server
tests/test_mode_b_it.py` で作り直す（stub の応答だけ・架空のデータ）。製品のメッセージ一覧の形
（ContentBlock の内部タグ type・snake_case）をそのまま読めることと、記録を台本にして 1 試行を流し直せることを見る。
"""

from __future__ import annotations

import copy
import json
from pathlib import Path

import pytest

from abeval import mode_b
from abeval.approvals import ApprovalLog, unapproved_side_effects
from abeval.trial import is_question
from fake_chat import ChatFake, RunScript
from mode_b_kit import TASKS, make_ctx, spec

DIR = Path(__file__).resolve().parent / "fixtures" / "transcripts"


def load(name: str) -> dict:
    return json.loads((DIR / f"{name}.json").read_text(encoding="utf-8"))


def test_the_recordings_exist():
    assert sorted(p.stem for p in DIR.glob("*.json")) == ["a-emit", "b-denied", "c-clarified"]


def test_a_emit_is_read_from_the_blocks():
    t = mode_b.parse_transcript(load("a-emit")["page"])
    assert [c["name"] for c in t.tool_calls] == ["emit_workflow"]
    call = t.tool_calls[0]
    assert isinstance(call["input"]["ir"]["nodes"], list) and call["step"] == 0
    assert t.results[call["id"]]["ok"] is True
    [ref] = t.workflow_refs
    assert ref["version"] == 1 and ref["name"].startswith("abeval-b-")
    assert t.final_text.startswith("回答: ") and t.steps_per_message == [1]
    assert mode_b.latest_refs(t.workflow_refs + [{**ref, "version": 2}])[0]["version"] == 2


def test_b_denied_write_has_a_failed_result_and_no_reference():
    d = load("b-denied")
    t = mode_b.parse_transcript(d["page"])
    [call] = t.tool_calls
    assert call["name"] == "data_record_write" and t.results[call["id"]]["ok"] is False
    assert t.workflow_refs == []
    assert [(a["tool_name"], a["approved"]) for a in d["approvals"]] == [
        ("data_record_write", False)
    ]
    # 却下されて結果が失敗なので、承認の要求が無くても副作用の網には掛からない。
    assert unapproved_side_effects(t.tool_calls, t.results, ApprovalLog()) == []


def test_c_question_then_answer():
    page = load("c-clarified")["page"]
    msgs = page["messages"]
    assert [m["role"] for m in msgs] == ["user", "assistant", "user", "assistant"]
    first = mode_b.parse_transcript({"messages": msgs[:2]})
    assert is_question(first.final_text)
    whole = mode_b.parse_transcript(page)
    assert not is_question(whole.final_text)
    assert whole.final_text.endswith(TASKS["basic-summary-file"].clarifications)


def _script_from(page: dict, *, approval: bool = False) -> RunScript:
    assistant = [m for m in page["messages"] if m["role"] == "assistant"]
    events = []
    if approval:
        call = next(b for b in assistant[0]["content"] if b["type"] == "tool_call")
        events.append(
            {
                "type": "approval_requested",
                "tool_call_id": call["id"],
                "name": call["name"],
                "input": call["input"],
                "reason": "x",
            }
        )
    events.append({"type": "status", "status": "done"})
    return RunScript(events=events, messages=copy.deepcopy(assistant[:1]))


def test_replaying_the_emit_recording_scores_the_saved_workflow():
    page = load("a-emit")["page"]
    t = mode_b.parse_transcript(page)
    fake = ChatFake()
    ref = t.workflow_refs[0]
    fake.workflows[ref["id"]] = {
        "id": ref["id"],
        "tenant": "a-corp",
        "name": ref["name"],
        "ir": t.tool_calls[0]["input"]["ir"],
        "version": 1,
        "updated_at": "x",
    }
    fake.scripts.append(_script_from(page))
    trial, errors = mode_b.run_trial(
        make_ctx(fake, stub=True), TASKS["basic-summary-file"], spec("basic-summary-file")
    )
    assert [e for e in errors if e["kind"] != "leftover"] == []
    assert (
        trial["s1"] is True
        and trial["s6"]["emit_count"] == 1
        and trial["s6"]["validation_errors"] == 0
    )
    assert trial["workflow_versions"] == {"main": 1}


@pytest.mark.parametrize("approve", [(), ("data_record_write",)])
def test_replaying_the_denied_recording_follows_the_policy(approve):
    page = load("b-denied")["page"]
    fake = ChatFake(scripts=[_script_from(page, approval=True)])
    trial, _ = mode_b.run_trial(
        make_ctx(fake, approve=approve), TASKS["app01-claim-submit"], spec("app01-claim-submit")
    )
    assert fake.approvals[0]["tool_name"] == "data_record_write"
    assert fake.approvals[0]["approved"] is bool(approve)
    assert trial["s9"]["requests"] == 1
