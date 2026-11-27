"""Mode B の基盤の失敗（採点もキャッシュもせず errors に入れる・費用の上限なら打ち切る）と、聞き返しの数え方・
キャッシュのキー（GAP-30 §4.7・§4.8）。偽の shiki・abeval.proxy（一時のポート）・偽の上流で通す。"""

from __future__ import annotations

import copy
import json

from abeval import grading, mode_b
from abeval.tasks import parse_task
from fake_chat import ChatFake, RunScript, text_message
from fake_upstream import FakeUpstream, Reply
from inproc import running_proxy
from mode_b_kit import TASKS, make_ctx, spec, tool_call, tool_result, workflow_ref

TASK = TASKS["basic-summary-file"]


def _saved_script(fake: ChatFake, text: str = "保存しました。") -> RunScript:
    wid = "00000000-0000-4000-8000-000000000001"
    fake.workflows[wid] = {
        "id": wid,
        "tenant": "a-corp",
        "name": "summary",
        "ir": TASK.reference_irs()["main"],
        "version": 1,
        "updated_at": "x",
    }
    return RunScript(
        messages=[
            {
                "role": "assistant",
                "content": [
                    tool_call("e1", "emit_workflow", {"ir": TASK.reference_irs()["main"]}),
                    tool_result("e1", True, "保存しました"),
                    workflow_ref(wid, "summary", 1),
                    {"type": "text", "text": text},
                ],
            }
        ]
    )


def test_an_error_event_is_an_llm_error_and_is_not_cached():
    def failing():
        return RunScript(
            events=[
                {"type": "error", "message": "応答の生成に失敗しました。もう一度お試しください。"},
                {"type": "status", "status": "failed"},
            ],
            messages=[text_message("")],
        )

    fake = ChatFake(scripts=[failing(), failing()])
    ctx = make_ctx(fake)
    trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert trial is None
    assert [e["kind"] for e in errors] == ["llm_error"]
    assert "応答の生成に失敗しました" in errors[0]["detail"] and not errors[0]["stop"]
    assert len(fake.posted) == 1  # 誤りで終わった会話に聞き返しの答えを送らない
    # キャッシュに無いので、もう一度流すと会話をやり直す（失敗が使い回されない）。
    again, errors2 = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert again is None and [e["kind"] for e in errors2] == ["llm_error"]
    assert len(fake.posted) == 2
    assert not list((ctx.cfg.paths.cache / "trials").glob("*.json"))


def test_a_question_after_saving_is_not_a_clarification():
    fake = ChatFake()
    fake.scripts.append(_saved_script(fake, "保存しました。自動実行を有効にしますか？"))
    ctx = make_ctx(fake)
    ctx.stub_plumbing = False
    trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert [e for e in errors if e["kind"] != "leftover"] == []
    assert len(fake.posted) == 1 and not trial["clarified"]
    assert trial["s8"]["asked"] is False and trial["s1"] is True


def test_a_run_case_that_cannot_run_is_not_scored(monkeypatch):
    fake = ChatFake()
    fake.scripts.append(_saved_script(fake))
    ctx = make_ctx(fake)
    monkeypatch.setattr(
        mode_b,
        "run_saved",
        lambda *a, **k: grading._summary([{"case": "normal", "error": "TimeoutError: 180 秒"}]),
    )
    trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert trial is None
    infra = [e for e in errors if e["kind"] not in ("leftover", "cleanup")]
    assert [e["kind"] for e in infra] == ["run_case"] and "TimeoutError" in infra[0]["detail"]
    assert "cost" in infra[0]
    assert not list((ctx.cfg.paths.cache / "trials").glob("*.json"))


def test_stub_plumbing_failure_is_not_scored(monkeypatch):
    fake = ChatFake()
    fake.scripts.append(_saved_script(fake))
    ctx = make_ctx(fake)
    monkeypatch.setattr(
        mode_b,
        "stub_plumbing",
        lambda *a, **k: {"error": "CaseError: 409", "cases": [], "s2d": None},
    )
    trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert trial is None
    assert "stub_plumbing" in [e["kind"] for e in errors]


class OtherModelChat(ChatFake):
    """製品が許可リストに無いモデルで送る（abeval.proxy が送る前に拒む）。"""

    def product_request(self, thread_id, text, body):
        return {**super().product_request(thread_id, text, body), "model": "claude-other-1"}


def test_a_proxy_rejection_inside_the_chat_stops_the_run():
    fake = OtherModelChat(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        fake.llm_url = proxy.url
        ctrl.register_run("r1", "e0", 1.0)
        ctx = make_ctx(fake, ctrl=ctrl, run_id="r1")
        specs = [spec("basic-summary-file", n) for n in range(3)]
        trials, errors = mode_b.run(ctx, specs, TASKS)
        assert up.received == []
        assert ctrl.spend()["runs"]["r1"]["rejected"] == 2  # 題名と本文の 2 回
    assert trials == []
    assert [e["kind"] for e in errors] == ["budget", "stopped", "stopped"]
    assert errors[0]["stop"] and "許可リスト" in errors[0]["detail"]
    assert len(fake.posted) == 1  # 打ち切った後は送らない


def test_the_local_relay_403_stops_the_run():
    def relay_cap(_req):
        body = {"type": "error", "error": {"type": "permission_error", "message": "relay: cap"}}
        return Reply(403, {"content-type": "application/json"}, [json.dumps(body).encode()])

    fake = ChatFake(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    with FakeUpstream(relay_cap) as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        fake.llm_url = proxy.url
        ctrl.register_run("r1", "e0", 1.0)
        ctx = make_ctx(fake, ctrl=ctrl, run_id="r1")
        trials, errors = mode_b.run(ctx, [spec("basic-summary-file", n) for n in range(2)], TASKS)
    assert trials == [] and [e["kind"] for e in errors] == ["budget", "stopped"]
    assert "403" in errors[0]["detail"]


def test_too_little_budget_left_stops_before_the_chat():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        ctrl.register_run("r1", "e0", 0.01)  # 1 試行の最悪（$0.05）より少ない
        ctx = make_ctx(fake, ctrl=ctrl, run_id="r1")
        trials, errors = mode_b.run(ctx, [spec("basic-summary-file", n) for n in range(2)], TASKS)
    assert trials == [] and fake.posted == [] and fake.threads == {}
    assert [e["kind"] for e in errors] == ["budget", "stopped"]
    assert "1 試行の最悪" in errors[0]["detail"]


def test_an_unregistered_run_stops_before_the_chat():
    fake = ChatFake()
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        ctx = make_ctx(fake, ctrl=ctrl, run_id="nope")
        trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert trial is None and errors[0]["kind"] == "budget" and errors[0]["stop"]


def test_the_cache_key_follows_the_grader_and_the_task_expectations():
    fake = ChatFake()
    ctx = make_ctx(fake)
    ts = spec("basic-summary-file")
    base = mode_b.key_for(ctx, TASK, ts)
    ctx.grader_sha = "other"
    assert mode_b.key_for(ctx, TASK, ts) != base
    ctx.grader_sha = "none"
    assert mode_b.key_for(ctx, TASK, ts) == base
    raw = copy.deepcopy(dict(TASK.raw))
    raw["run_cases"][0]["expect"]["files"][0]["count"] = 2
    assert mode_b.key_for(ctx, parse_task(raw), ts) != base
    # 依頼文の参考情報は、キーでは置き場所の印のまま（実験ごとに id が変わってもキーが同じ）。
    assert "@folder:out" in mode_b.key_prompt(TASK, ts)
