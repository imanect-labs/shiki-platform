"""キャッシュのキーが結果を左右するものをすべて含むこと（BUG-249）。

- Mode B: 課題の `clarifications`・`ask_terms`、承認の方針（`[mode_b] approve_tools` と副作用のあるツールの組）、
  版のサーバーの 1 発話の予算、stub の配管を流すか、採点の後の hook。どれかを変えたら前の結果を返さない。
- Mode A: 送る model もキーに入れる。生成の結果の分け方（聞き返し）は課題の `ask_terms` に依るので、キャッシュから
  読んだときに今の課題で分け直す（キーに入れて生成し直すのではなく、採点の側でやり直す）。
"""

from __future__ import annotations

import copy
import dataclasses

from test_mode_a import capture_obj

from abeval import approvals, mode_a, mode_b
from abeval.approvals import ApprovalPolicy
from abeval.cache import TrialCache
from abeval.config import load_config
from abeval.tasks import parse_task
from fake_chat import ChatFake, RunScript, text_message
from mode_b_kit import TASKS, make_ctx, spec

TASK = TASKS["basic-summary-file"]


def _task(**changes) -> object:
    raw = copy.deepcopy(dict(TASK.raw))
    raw.update(changes)
    return parse_task(raw)


def test_mode_b_key_follows_the_conditions_that_change_the_result(monkeypatch):
    ctx = make_ctx(ChatFake())
    ts = spec("basic-summary-file")
    base = mode_b.key_for(ctx, TASK, ts)
    assert mode_b.key_for(ctx, TASK, ts) == base
    # 課題の聞き返しへの答えと、聞き返しと判定する語。
    assert mode_b.key_for(ctx, _task(clarifications="保存先は「別」です。"), ts) != base
    assert mode_b.key_for(ctx, _task(ask_terms=["教えてください"]), ts) != base
    # 承認の方針（許可リスト・副作用のあるツールの組）。
    changed = []
    for name, value in [
        ("policy", ApprovalPolicy(frozenset({"data_record_write"}))),
        ("budget_micros", 300_000),
        ("stub_plumbing", False),
        ("hooks", [lambda inp: {}]),
    ]:
        other = dataclasses.replace(ctx, **{name: value})
        changed.append((name, mode_b.key_for(other, TASK, ts) != base))
    assert all(c for _, c in changed), changed
    monkeypatch.setattr(mode_b, "SIDE_EFFECT_TOOLS", frozenset({"data_record_write", "x"}))
    assert mode_b.key_for(ctx, TASK, ts) != base
    monkeypatch.undo()
    # 結果を左右しないもの（読み直しの間隔）では変わらない。
    cfg = dataclasses.replace(ctx.cfg, mode_b=dataclasses.replace(ctx.cfg.mode_b, poll_secs=5.0))
    assert mode_b.key_for(dataclasses.replace(ctx, cfg=cfg), TASK, ts) == base


def test_mode_b_does_not_reuse_a_result_scored_under_another_approval_policy():
    script = lambda: RunScript(messages=[text_message("作れませんでした。")])  # noqa: E731
    fake = ChatFake(scripts=[script(), script(), script()])
    ctx = make_ctx(fake)
    trial, _ = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert trial is not None and not trial.get("from_cache")
    again, _ = mode_b.run_trial(ctx, TASK, spec("basic-summary-file"))
    assert again["from_cache"] and len(fake.posted) == 1
    allowed = dataclasses.replace(ctx, policy=ApprovalPolicy(frozenset({"data_record_write"})))
    third, _ = mode_b.run_trial(allowed, TASK, spec("basic-summary-file"))
    assert not third.get("from_cache") and len(fake.posted) == 2
    budget = dataclasses.replace(ctx, budget_micros=999)
    fourth, _ = mode_b.run_trial(budget, TASK, spec("basic-summary-file"))
    assert not fourth.get("from_cache") and len(fake.posted) == 3


def test_the_side_effect_tools_are_the_approvals_module_ones():
    assert mode_b.SIDE_EFFECT_TOOLS is approvals.SIDE_EFFECT_TOOLS


def _mode_a_ctx() -> mode_a.ModeAContext:
    cfg = load_config()
    return mode_a.ModeAContext(
        cfg=cfg,
        client=None,  # type: ignore[arg-type]（キャッシュから読むだけ）
        anthropic=None,
        capture=capture_obj(),
        lint=lambda items: {},
        catalog=None,  # type: ignore[arg-type]
        cache=TrialCache(cfg.paths.cache),
        binary_sha="b" * 64,
        config_sha="c" * 64,
        generate=False,
    )


def test_mode_a_key_has_the_model(monkeypatch):
    ctx = _mode_a_ctx()
    ts = mode_a.TrialSpec("e0", "a", "v0", TASK.id, "flow", 0, "medium")
    base = mode_a.key_for(ctx, TASK, ts)
    monkeypatch.setattr(mode_a, "MODEL", "claude-other")
    assert mode_a.key_for(ctx, TASK, ts) != base


def test_mode_a_reclassifies_a_cached_generation_with_the_current_ask_terms():
    ctx = _mode_a_ctx()
    ts = mode_a.TrialSpec("e0", "a", "v0", TASK.id, "flow", 0, "medium")
    key = mode_a.key_for(ctx, TASK, ts)
    gen = {"status": "no_emit", "stop_reason": "end_turn", "irs": [], "emit_count": 0}
    response = [{"type": "text", "text": "保存先のフォルダーを教えてください"}]
    ctx.cache.put(key, {"key": key, "gen": gen, "response": response})
    got = mode_a.generate(ctx, TASK, None, ts)  # type: ignore[arg-type]
    assert got["from_cache"] and got["gen"]["status"] == "no_emit"
    asking = _task(ask_terms=["教えてください"])
    assert mode_a.key_for(ctx, asking, ts) == key  # 生成はし直さない（分け方だけ変わる）
    got = mode_a.generate(ctx, asking, None, ts)  # type: ignore[arg-type]
    assert got["from_cache"] and got["gen"]["status"] == "asked"
    # 応答の無い古い記録はそのまま。
    assert mode_a._reclassify(gen, None, asking) == gen
