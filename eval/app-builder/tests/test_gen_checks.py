"""課題の検査 G2・G4・G5・G7・G8・G9 の合格の例と不合格の例（拒否リストは架空の語の一覧）。"""

from __future__ import annotations

import copy

import pytest

import recorded
from abeval import gen_checks as g
from abeval.tasks import parse_task

# 架空の語（テスト専用。本物の拒否リストはリポジトリの外に置く）。
FICTIONAL = ["架空商事", "ほげ省", "Zorblax"]


def _task(**phr):
    base = copy.deepcopy(recorded.tasks()["basic-summary-file"].raw)
    base["phrasings"].update(phr)
    return parse_task(base)


def test_g4_passes_for_plain_japanese():
    assert g.g4_no_leak(_task(), recorded.catalog()).ok


@pytest.mark.parametrize(
    "text",
    [
        "llm.invoke で要約して storage.write で保存して",
        '{"$from": "input"} を使う',
        "件名は $template で組む",
        "分岐の true の先に書く",
        "on_error を continue にして",
        '結果は ["a", "b"] のように',
        "error の出口につなぐ",
    ],
)
def test_g4_catches_answer_leaks(text):
    r = g.g4_no_leak(_task(app=text), recorded.catalog())
    assert not r.ok and r.problems, text


def test_g4_port_names_need_word_boundaries():
    # 英単語の一部（output・errors など）は出口の名前に数えない。
    assert g.g4_no_leak(
        _task(app="件名と本文を output フォルダの errors に"), recorded.catalog()
    ).ok


def test_g4_checks_clarifications_too():
    raw = copy.deepcopy(recorded.tasks()["basic-summary-file"].raw)
    raw["clarifications"] = "storage.write の folder に入れて"
    r = g.g4_no_leak(parse_task(raw), recorded.catalog())
    assert not r.ok and any("clarifications" in p for p in r.problems)


def test_g5_needs_a_term_for_every_input_in_every_phrasing():
    assert g.g5_values_named(_task()).ok
    r = g.g5_values_named(_task(auto="中身を短くして『要約』に置いてください"))
    assert not r.ok
    assert any("'title'" in p and "auto" in p for p in r.problems)
    assert any("'body'" in p for p in r.problems)


def test_g7_jaccard():
    assert g.jaccard("あいうえお", "あいうえお") == 1.0
    assert g.jaccard("", "") == 1.0
    assert g.jaccard("あいうえおかきくけこ", "さしすせそたちつてと") == 0.0
    tasks = list(recorded.tasks().values())
    assert all(r.ok for r in g.g7_not_duplicated(tasks))
    dup = _task(app=recorded.tasks()["basic-summary-file"].phrasings["flow"] + "。")
    bad = g.g7_not_duplicated([dup])
    assert not bad[0].ok and "Jaccard" in bad[0].problems[0]
    # 既存の依頼文とも比べる（生成した課題の検査）。
    other = g.g7_not_duplicated([_task()], existing=[("old", _task().phrasings["app"])])
    assert not other[0].ok


def test_g8_with_a_fictional_denylist(tmp_path, monkeypatch):
    p = tmp_path / "deny.txt"
    p.write_text("# テスト用の架空の語\n" + "\n".join(FICTIONAL) + "\n\n", encoding="utf-8")
    monkeypatch.setenv("ABEVAL_DENYLIST", str(p))
    words, why = g.load_denylist()
    assert why is None and words == [w.casefold() for w in FICTIONAL]
    assert g.g8_fictional(_task(), words).ok
    bad = g.g8_fictional(_task(app="架空商事の経費を要約して"), words)
    assert not bad.ok
    # 依頼文の外（fixture・正解の IR）も見る。大文字小文字は区別しない。
    raw = copy.deepcopy(recorded.tasks()["basic-summary-file"].raw)
    raw["fixtures"]["folders"][0]["name"] = "zorblax の資料"
    r = g.g8_fictional(parse_task(raw), words)
    assert not r.ok and r.problems == [
        "/fixtures/folders/0/name: 拒否リストの語を含む（7 文字の語）"
    ]
    for res in (bad, r):
        assert not any(w.casefold() in p.casefold() for p in res.problems for w in FICTIONAL), (
            "語そのものを理由に出さない"
        )


def test_g8_is_skipped_with_a_reason_when_the_list_is_missing(tmp_path, monkeypatch):
    monkeypatch.setenv("ABEVAL_DENYLIST", str(tmp_path / "none.txt"))
    words, why = g.load_denylist()
    assert words is None and "飛ばします" in why
    r = g.g8_fictional(_task(app="架空商事"), words, why)
    assert r.ok and r.skipped == why


def test_g9_requires_against_version_features():
    cat = recorded.catalog()
    feats = g.version_features(cat, {"scheduled_at": True, "trigger": False})
    assert "node:llm.invoke" in feats and "cap:scheduled_at" in feats and "cap:trigger" not in feats
    assert "node:notify.send" not in feats, "使えないノードは入れない"
    t = recorded.tasks()
    assert g.g9_supported(t["basic-summary-file"], feats).ok
    r = g.g9_supported(t["app09-minutes-tasks"], feats)
    assert (
        not r.ok
        and any("trigger:event" in p for p in r.problems)
        and any("cap:trigger" in p for p in r.problems)
    )
    assert g.g9_supported(
        t["app09-minutes-tasks"],
        g.version_features(cat, {"trigger": True}, triggers=("interactive", "event")),
    ).ok


def test_g2_with_recorded_lint():
    tasks = list(recorded.tasks().values())
    lint = recorded.lint()
    assert all(r.ok for r in g.g2_reference_passes(tasks, recorded.catalog(), lint))
    # 誤りのある結果を渡すと落ちる。
    broken = dict(lint)
    key = "basic-summary-file/main"
    broken[key] = recorded.lint()[f"{key}/mut:unknown_var"]
    res = {r.task_id: r for r in g.g2_reference_passes(tasks, recorded.catalog(), broken)}
    assert not res["basic-summary-file"].ok and "S2s" in res["basic-summary-file"].problems[0]
    del broken[key]
    res = {r.task_id: r for r in g.g2_reference_passes(tasks, recorded.catalog(), broken)}
    assert "結果がありません" in res["basic-summary-file"].problems[0]


def test_verify_offline_runs_everything_and_skips_g8_without_a_list(tmp_path, monkeypatch):
    monkeypatch.setenv("ABEVAL_DENYLIST", str(tmp_path / "none.txt"))
    tasks = list(recorded.tasks().values())
    out = g.verify_offline(
        tasks,
        recorded.catalog(),
        lint=recorded.lint(),
        features=g.version_features(
            recorded.catalog(),
            {"scheduled_at": True, "trigger": True, "fsm": True},
            triggers=("interactive", "schedule", "event"),
        ),
    )
    checks = {r.check for r in out}
    assert checks == {"G2", "G4", "G5", "G7", "G8", "G9"}
    assert all(r.ok for r in out), [r.to_dict() for r in out if not r.ok]
    assert all(r.skipped for r in out if r.check == "G8")


def test_g9_tasks_with_a_state_flow_need_the_fsm_routes():
    """fixture で状態の進め方を作る課題は、その口（cap:fsm）の無い版では対象外になる（GAP-113）。"""
    tasks = recorded.tasks()
    fsm_tasks = {
        tid for tid, t in tasks.items() if any("fsm" in x for x in t.fixtures.get("tables") or [])
    }
    assert fsm_tasks == {
        "app01-posting-batch",
        "app02-triage",
        "app05-screening-text",
        "app11-checkout",
        "app11-return",
        "app12-faq-table",
    }
    caps = {"scheduled_at": True, "trigger": True}
    without = g.version_features(recorded.catalog(), caps)
    with_fsm = g.version_features(recorded.catalog(), {**caps, "fsm": True})
    for tid in fsm_tasks:
        assert {"cap:fsm", "node:data.transition"} <= set(tasks[tid].requires), tid
        assert g.g9_supported(tasks[tid], without).problems == ["版に無い: cap:fsm"], tid
        assert g.g9_supported(tasks[tid], with_fsm).ok, tid
    # 状態の進め方を作らない課題は cap:fsm を求めない。
    assert not any("cap:fsm" in t.requires for tid, t in tasks.items() if tid not in fsm_tasks)
