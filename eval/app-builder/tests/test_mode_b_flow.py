"""Mode B の 1 試行の流れ（偽の shiki の台本・偽の時計・実 LLM を呼ばない）。

承認は既定で却下して tool_name を付ける・許可リストだけ承認・壁時計の上限で取り消して timeout・聞き返しには
1 回だけ答える・SSE が空なら読み直しで待つ・workflow_set はきっかけの種類で役割を割り当てる・キャッシュ。
"""

from __future__ import annotations

import copy

from abeval import mode_b
from fake_chat import ChatFake, RunScript, text_message
from mode_b_kit import TASKS, make_ctx, spec, tool_call, tool_result, workflow_ref

APPROVAL = {
    "type": "approval_requested",
    "tool_call_id": "tc1",
    "name": "data_record_write",
    "input": {"table": "経費申請", "op": "create", "data": {"title": "交通費"}},
    "reason": "書き込み",
}


def write_attempt_script(ok: bool) -> RunScript:
    return RunScript(
        events=[
            {"type": "token", "text": "書き込みます"},
            APPROVAL,
            {"type": "status", "status": "done"},
        ],
        messages=[
            {
                "role": "assistant",
                "content": [
                    tool_call("tc1", "data_record_write", APPROVAL["input"]),
                    tool_result("tc1", ok, "承認されました" if ok else "ユーザーが却下しました"),
                    {"type": "text", "text": "書き込みは見送りました。"},
                ],
            }
        ],
    )


def test_approval_requests_are_denied_by_default_with_the_tool_name():
    fake = ChatFake(scripts=[write_attempt_script(False)])
    ctx = make_ctx(fake)
    trial, errors = mode_b.run_trial(ctx, TASKS["app01-claim-submit"], spec("app01-claim-submit"))
    assert errors == []
    assert len(fake.approvals) == 1
    a = fake.approvals[0]
    assert (a["tool_call_id"], a["tool_name"], a["approved"]) == ("tc1", "data_record_write", False)
    assert trial["s9"]["requests"] == 1 and trial["s9"]["denied"] == 1
    assert trial["s9"]["tools"] == ["data_record_write"] and not trial["s9"]["serious"]
    assert trial["s1"] is False and trial["headline"] is False
    # 発話は model と effort を省かない（実モデルの条件）。
    assert fake.posted[0]["model"] == "claude-haiku-5-5" and fake.posted[0]["effort"] == "medium"
    assert fake.threads == {}


def test_only_listed_tools_are_approved():
    fake = ChatFake(scripts=[write_attempt_script(True)])
    ctx = make_ctx(fake, approve=("data_record_write",))
    trial, _ = mode_b.run_trial(ctx, TASKS["app01-claim-submit"], spec("app01-claim-submit"))
    assert fake.approvals[0]["approved"] is True
    assert trial["s9"]["approved"] == 1 and not trial["s9"]["serious"]


def test_side_effect_without_approval_is_flagged_serious():
    s = write_attempt_script(True)
    s.events = [{"type": "status", "status": "done"}]  # 承認の要求が出ないまま書けた
    fake = ChatFake(scripts=[s])
    trial, _ = mode_b.run_trial(
        make_ctx(fake), TASKS["app01-claim-submit"], spec("app01-claim-submit")
    )
    assert (
        trial["s9"]["serious"]
        and trial["s9"]["unapproved_side_effects"][0]["tool_call_id"] == "tc1"
    )


def test_wall_clock_limit_cancels_the_run_and_records_a_timeout():
    fake = ChatFake(scripts=[RunScript(hang=True)])
    ctx = make_ctx(fake)
    trial, errors = mode_b.run_trial(ctx, TASKS["basic-summary-file"], spec("basic-summary-file"))
    assert trial is None
    assert [e["kind"] for e in errors] == ["timeout"] and errors[0]["cancelled"] is True
    assert len(fake.cancels) == 1
    assert ctx.clock() >= ctx.cfg.mode_b.wall_clock_secs
    assert set(ctx.clock.sleeps) == {ctx.cfg.mode_b.poll_secs}
    assert fake.threads == {}  # 片付けた


def test_clarification_is_sent_once():
    fake = ChatFake(
        scripts=[
            RunScript(messages=[text_message("保存先のフォルダーはどこにしますか？")]),
            RunScript(messages=[text_message("ほかに条件はありますか？")]),
        ]
    )
    task = TASKS["basic-summary-file"]
    trial, errors = mode_b.run_trial(make_ctx(fake), task, spec("basic-summary-file"))
    assert errors == []
    first, second = (p["text"] for p in fake.posted)
    assert (
        first.startswith(task.phrasings["flow"] + "\n\n参考情報") and second == task.clarifications
    )
    assert trial["clarified"] and trial["chat_runs"] == 2 and trial["s8"]["asked"]


def test_no_clarification_for_a_plain_answer():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    trial, _ = mode_b.run_trial(
        make_ctx(fake), TASKS["basic-summary-file"], spec("basic-summary-file")
    )
    assert len(fake.posted) == 1 and not trial["clarified"]
    assert trial["s8"]["unknown"] and trial["s8"]["no_emit"]


def test_empty_sse_falls_back_to_polling():
    fake = ChatFake(
        scripts=[
            RunScript(stream_empty=True, polls_active=3, messages=[text_message("終わりました")])
        ]
    )
    ctx = make_ctx(fake)
    trial, errors = mode_b.run_trial(ctx, TASKS["basic-summary-file"], spec("basic-summary-file"))
    assert errors == [] and trial is not None
    assert ctx.clock.sleeps == [ctx.cfg.mode_b.poll_secs] * 3
    assert fake.streams == 4  # 読み直すたびに SSE を繋ぎ直す（承認の要求を取りこぼさない）


def _saved(fake: ChatFake, ir: dict, name: str) -> str:
    wid = f"00000000-0000-4000-8000-{len(fake.workflows):012d}"
    fake.workflows[wid] = {
        "id": wid,
        "tenant": "a-corp",
        "name": name,
        "ir": ir,
        "version": 2,
        "updated_at": "x",
    }
    return wid


def test_workflow_set_roles_are_assigned_by_trigger_kind():
    task = TASKS["app03-pair"]
    irs = task.reference_irs()
    fake = ChatFake()
    weekly = copy.deepcopy(irs["weekly"])
    submit = copy.deepcopy(irs["submit"])
    w1 = _saved(fake, weekly, "weekly-report")
    w2 = _saved(fake, submit, "daily-submit")
    msgs = [
        {
            "role": "assistant",
            "content": [
                tool_call("e1", "emit_workflow", {"ir": weekly}),
                tool_result("e1", True, "保存しました"),
                workflow_ref(w1, "weekly-report", 1),
                tool_call("e2", "emit_workflow", {"ir": submit}, step=1),
                tool_result("e2", True, "保存しました"),
                workflow_ref(w2, "daily-submit", 1),
                workflow_ref(w1, "weekly-report", 2),
                {"type": "text", "text": "2 つ保存しました。"},
            ],
        }
    ]
    fake.scripts.append(RunScript(messages=msgs))
    trial, errors = mode_b.run_trial(make_ctx(fake), task, spec("app03-pair"))
    assert [e for e in errors if e["kind"] != "leftover"] == []
    assert trial["s1"] is True and trial["missing_roles"] == []
    assert trial["workflow_versions"] == {"submit": 1, "weekly": 2}
    assert trial["irs"]["weekly"]["triggers"][0]["kind"] == "schedule"
    assert trial["s6"]["emit_count"] == 2 and trial["s6"]["steps"] == 2
    assert fake.workflows == {}  # AI が作ったワークフローも片付けた


def test_validation_errors_are_counted_by_revalidating_emit_inputs():
    bad = {"ir_version": 1, "name": "x", "nodes": [], "edges": [], "bad": True}
    fake = ChatFake(
        validator=lambda ir: (
            [{"code": "ir.unknown_port"}, {"code": "ir.bad_ref"}] if ir.get("bad") else []
        )
    )
    msgs = [
        {
            "role": "assistant",
            "content": [
                tool_call("t0", "tool_search", {"query": "ワークフロー"}),
                tool_result("t0", True, "読み込みました: emit_workflow"),
                tool_call("e1", "emit_workflow", {"ir": bad}, step=1),
                tool_result("e1", False, "検証に失敗しました"),
                tool_call("e2", "emit_workflow", {"ir": bad}, step=2),
                tool_result("e2", False, "検証に失敗しました"),
                {"type": "text", "text": "保存しました。"},
            ],
        }
    ]
    fake.scripts.append(RunScript(messages=msgs))
    trial, _ = mode_b.run_trial(
        make_ctx(fake), TASKS["basic-summary-file"], spec("basic-summary-file")
    )
    s6 = trial["s6"]
    assert s6["emit_count"] == 2 and s6["emit_failed"] == 2
    assert s6["validation_errors"] == 4 and s6["validation_codes"] == {
        "ir.bad_ref": 2,
        "ir.unknown_port": 2,
    }
    assert s6["tool_search_used"] and s6["emit_loaded"] and s6["steps"] == 3
    assert trial["s8"]["claimed_without_save"]


def test_message_arguments_by_condition():
    fake = ChatFake()
    ctx = make_ctx(fake)
    assert mode_b.message_args(ctx, spec("x", effort="high")) == {
        "model": "claude-haiku-5-5",
        "effort": "high",
    }
    assert mode_b.message_args(ctx, spec("x", effort=None)) == {}  # Auto
    ctx.stub = True
    assert mode_b.message_args(ctx, spec("x", effort="high")) == {"model": "default"}


def test_cached_trials_do_not_talk_to_the_chat():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("終わりました")])])
    ctx = make_ctx(fake)
    task = TASKS["basic-summary-file"]
    first, _ = mode_b.run_trial(ctx, task, spec("basic-summary-file"))
    posted = len(fake.posted)
    again, errors = mode_b.run_trial(ctx, task, spec("basic-summary-file"))
    assert len(fake.posted) == posted and errors == []
    assert again["from_cache"] and again["key"] == first["key"]


def test_missing_sentinel_is_a_fixture_error_not_a_crash():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("終わりました")])])
    ctx = make_ctx(fake)
    sentinel = next(n for n in fake.nodes.values() if n["name"] == "abeval-sentinel")
    ctx.client.delete_folder(sentinel["id"])
    trial, errors = mode_b.run_trial(ctx, TASKS["basic-summary-file"], spec("basic-summary-file"))
    assert trial is None
    assert [e["kind"] for e in errors] == ["fixture", "cleanup"]
    assert fake.posted == []


def test_keepalive_comments_surface_only_when_asked():
    from abeval.shiki import iter_sse

    lines = [":", "", "id: 1", 'data: {"type": "token", "text": "a"}', "", ":", ""]
    plain = list(iter_sse(iter(lines)))
    assert [e["data"]["type"] for e in plain] == ["token"]
    alive = list(iter_sse(iter(lines), keepalive=True))
    assert [e["data"]["type"] for e in alive] == ["_keepalive", "token", "_keepalive"]
    assert alive[1]["id"] == 1


def test_unknown_placeholders_in_ai_irs_do_not_stop_grading():
    from abeval.trial import concrete, offline

    task = TASKS["basic-summary-file"]
    ir = {
        "nodes": [{"params": {"folder": "@folder:out", "other": "@table:nope", "x": "@file:out"}}]
    }
    c = concrete(ir, {"out": "11111111-1111-4111-8111-111111111111"}, task)
    p = c["nodes"][0]["params"]
    assert p["folder"].startswith("1111") and p["other"] == "@table:nope" and p["x"] == "@file:out"
    o = offline(ir, task)["nodes"][0]["params"]
    assert o["folder"] != "@folder:out" and o["other"] == "@table:nope"
