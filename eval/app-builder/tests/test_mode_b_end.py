"""Mode B の会話の終わりの確かめ（BUG-248）。SSE が空で終わった・切れたときも、run の終わりの状態を確かめてから
採点する。

製品には run の状態を読む口（`GET /threads/{id}/runs/{run_id}` など）が無い（`crates/api/src/routes/chat*.rs`）。
SSE は、スレッドの最新の run が終わっていれば残りのイベントを流して閉じるので、終わりのイベントを読めていなければ
`last_event_id` から読み直す。それでも読めなければ、最後の user の発話の後に assistant の返事が無いことを失敗の印に
して、採点もキャッシュもせず errors（kind: no_reply）に入れる。
"""

from __future__ import annotations

from abeval import mode_b
from fake_chat import ChatFake, RunScript, text_message
from mode_b_kit import TASKS, make_ctx, spec

TASK = TASKS["basic-summary-file"]
FAILED = [
    {"type": "error", "message": "応答の生成に失敗しました。もう一度お試しください。"},
    {"type": "status", "status": "failed"},
]
APPROVAL = {
    "type": "approval_requested",
    "tool_call_id": "tc1",
    "name": "data_record_write",
    "input": {"table": "経費申請"},
}


def _run(fake: ChatFake):
    ctx = make_ctx(fake)
    trial, errors = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    return ctx, trial, errors


def _not_cached(ctx) -> bool:
    return ctx.cache.get(mode_b.key_for(ctx, TASK, spec("basic-summary-file"))) is None


def test_a_failure_that_ended_before_the_sse_connected_is_not_missed():
    # 繋いだ最初の SSE は空（run はもう失敗で終わっていた）。読み直すと終わりのイベントが流れる。
    fake = ChatFake(scripts=[RunScript(events=list(FAILED), empty_connects=1)])
    ctx, trial, errors = _run(fake)
    assert trial is None
    assert [e["kind"] for e in errors] == ["llm_error"], errors
    assert "応答の生成に失敗しました" in errors[0]["detail"]
    assert fake.streams == 2 and len(fake.posted) == 1  # 聞き返しの答えも送らない
    assert _not_cached(ctx)


def test_a_failure_while_the_sse_was_cut_is_not_missed():
    # 1 件目のイベントの後で SSE が切れ、切れている間に run が失敗で終わった。続きから読み直す。
    script = RunScript(events=[{"type": "token", "text": "作ります"}, *FAILED], cut_after=1)
    fake = ChatFake(scripts=[script])
    ctx, trial, errors = _run(fake)
    assert trial is None
    assert [e["kind"] for e in errors] == ["llm_error"], errors
    assert fake.streams == 2
    assert _not_cached(ctx)


def test_an_unconfirmed_end_without_a_reply_is_not_scored():
    # SSE が何度読んでも空（終わりの状態を確かめられない）で、最後の発話に返事が無い（中身の無い枠だけ）。
    fake = ChatFake(scripts=[RunScript(stream_empty=True)])
    ctx, trial, errors = _run(fake)
    assert trial is None
    assert [e["kind"] for e in errors] == ["no_reply"], errors
    assert not errors[0].get("stop")
    assert fake.streams == 1 + mode_b.END_REPLAYS
    assert _not_cached(ctx)
    assert fake.threads == {}  # 片付けた


def test_an_unconfirmed_end_with_a_reply_is_scored_and_marked_but_not_cached():
    """返事があれば採点するが、終わりを確かめられなかった試行はキャッシュに置かない（製品の中で失敗した run を
    モデルの失敗として残し続けず、次に流すときに測り直す）。"""
    fake = ChatFake(
        scripts=[RunScript(stream_empty=True, messages=[text_message("作れませんでした。")])]
    )
    ctx, trial, errors = _run(fake)
    assert errors == [] and trial is not None
    assert trial["chat_end_unconfirmed"] == 1 and trial["not_cached"] == "chat_end_unconfirmed"
    assert _not_cached(ctx)
    # 次に流すと測り直す（キャッシュから返さない）。
    fake2 = ChatFake(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    trial2, errors2 = mode_b.run_trial(ctx_with(fake2, ctx), TASK, spec("basic-summary-file"))
    assert errors2 == [] and not trial2.get("from_cache") and len(fake2.posted) == 1
    assert trial2["chat_end_unconfirmed"] == 0 and "not_cached" not in trial2
    assert not _not_cached(ctx)


def ctx_with(fake: ChatFake, like) -> mode_b.ModeBContext:
    """同じキャッシュを使う別の会話の文脈。"""
    ctx = make_ctx(fake)
    ctx.cache = like.cache
    return ctx


def test_a_confirmed_end_needs_no_replay():
    fake = ChatFake(scripts=[RunScript(messages=[text_message("作れませんでした。")])])
    _, trial, errors = _run(fake)
    assert errors == [] and trial["chat_end_unconfirmed"] == 0
    assert fake.streams == 1


def test_a_silent_failure_after_the_clarification_is_not_scored():
    # 1 回目は聞き返し（終わった）、聞き返しへの答えの run は SSE が空のまま返事を書かずに終わった。
    fake = ChatFake(
        scripts=[
            RunScript(messages=[text_message("保存先のフォルダーはどこにしますか？")]),
            RunScript(stream_empty=True),
        ]
    )
    ctx, trial, errors = _run(fake)
    assert trial is None and [e["kind"] for e in errors] == ["no_reply"], errors
    assert len(fake.posted) == 2
    assert _not_cached(ctx)


def test_approval_requests_read_after_the_run_ended_are_not_answered():
    fake = ChatFake(scripts=[RunScript(events=[APPROVAL, *FAILED], empty_connects=1)])
    _, trial, errors = _run(fake)
    assert fake.approvals == []  # 終わった run に承認を送らない
    assert trial is None and [e["kind"] for e in errors] == ["llm_error"]


def test_has_reply_looks_after_the_last_user_message():
    def page(*msgs):
        return {"messages": list(msgs)}

    user = {"role": "user", "content": [{"type": "text", "text": "作って"}]}
    empty = {"role": "assistant", "content": []}
    thinking = {"role": "assistant", "content": [{"type": "thinking", "text": "考える"}]}
    blank = {"role": "assistant", "content": [{"type": "text", "text": "  "}]}
    reply = text_message("できました")
    tool = {"role": "assistant", "content": [{"type": "tool_call", "id": "t", "name": "x"}]}
    assert mode_b.has_reply(page(user, reply))
    assert mode_b.has_reply(page(user, tool))
    assert not mode_b.has_reply(page(user, empty))
    assert not mode_b.has_reply(page(user, thinking))
    assert not mode_b.has_reply(page(user, blank))
    assert not mode_b.has_reply(page(user, reply, user, empty))  # 前の発話への返事は数えない
    assert not mode_b.has_reply(page())


def test_without_the_replay_the_early_failure_is_missed(monkeypatch):
    """読み直しを外すと、繋ぐ前に終わった失敗を見落とす（上のテストが読み直しを見ている）。"""
    monkeypatch.setattr(mode_b, "END_REPLAYS", 0)
    fake = ChatFake(scripts=[RunScript(events=list(FAILED), empty_connects=1)])
    _, trial, errors = _run(fake)
    assert [e["kind"] for e in errors] == ["no_reply"], errors


def test_without_the_reply_check_a_silent_failure_is_scored(monkeypatch):
    monkeypatch.setattr(mode_b, "has_reply", lambda page: True)
    fake = ChatFake(scripts=[RunScript(stream_empty=True)])
    _, trial, errors = _run(fake)
    assert trial is not None and errors == []
