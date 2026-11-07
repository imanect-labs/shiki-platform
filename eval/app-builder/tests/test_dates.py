"""日付の決め打ちの検査（BUG-247）: IR の中の日付の文字・実行の日に決まる値を持つ課題・Mode B の予定の週。"""

from __future__ import annotations

import copy
import datetime as dt

import pytest

import recorded
from abeval import mode_b
from abeval.grade import dates, mutate, static
from abeval.grade.dates import date_literals, in_window, ir_date_literals, scheduled_window
from abeval.grade.static import LintResult, date_literal_findings, grade_static
from abeval.tasks import Expect, offline_irs, parse_task
from fake_chat import ChatFake
from mode_b_kit import TASKS, make_ctx, spec

JST = dt.timezone(dt.timedelta(hours=9))


@pytest.mark.parametrize(
    ("text", "want"),
    [
        ("2026-11-07", ["2026-11-07"]),
        ("週報-2026/11/02.md", ["2026/11/02"]),
        ("2026.11.7 の分", ["2026.11.7"]),
        ("2026年11月7日の日報", ["2026年11月7日"]),
        ("11月7日（土）", ["11月7日"]),
        ("２０２６年１１月７日", ["2026年11月7日"]),  # 全角は NFKC でそろえる
        ("２０２６－１１－０７", ["2026-11-07"]),
        ("週報-20261102.md", ["20261102"]),
        ("from 2026-11-02 to 2026-11-08", ["2026-11-02", "2026-11-08"]),
        ("2026-11-09T09:00:00+09:00", ["2026-11-09"]),
    ],
)
def test_date_literals_find_the_usual_forms(text, want):
    assert date_literals(text) == want


@pytest.mark.parametrize(
    "text",
    [
        "要約-{date}.md",
        "3 行で要約してください",
        "金額は 120000 円",
        "版 1.2.3",
        "2026-13-01",  # 月が 13
        "11月40日",  # 日が 40
        "毎月25日に締める",  # 月の数字が無い
        "1000.5.3",  # 年が 19xx・20xx でない
        "6f2c3c1e-7a51-4b0e-9d3a-3b0a9b6c1e01",  # UUID
        "20261340",  # 8 桁でも日付にならない
        "",
    ],
)
def test_date_literals_ignore_non_dates(text):
    assert date_literals(text) == []


def test_ir_date_literals_look_at_params_but_not_references():
    ir = {
        "name": "weekly-2026-11-02",  # 名前は見ない（設定だけ）
        "nodes": [
            {
                "id": "save",
                "type": "storage.write",
                "params": {
                    "name": {
                        "$template": "週報-{d}.md",
                        "vars": {"d": {"$from": "run", "path": "/date"}},
                    },
                    "content": {"$template": "{x}（2026年11月2日〜8日）", "vars": {}},
                    "folder": {"$from": "input", "path": "/folder", "default": "2026-11-09"},
                },
            },
            {
                "id": "q",
                "type": "data.query",
                "params": {
                    "query": {"filter": {"cmp": {"field": "d", "op": "gte", "value": "2026-11-02"}}}
                },
            },
            {
                "id": "s",
                "type": "script.run",
                "params": {"source": {"inline": "var d = '2026/11/02';"}},
            },
        ],
    }
    hits = ir_date_literals(ir)
    assert hits == [
        ("save", "/params/content/$template", "2026年11月2日"),
        ("save", "/params/folder/default", "2026-11-09"),
        ("q", "/params/query/filter/cmp/value", "2026-11-02"),
        ("s", "/params/source/inline", "2026/11/02"),
    ]


def _expect(dated: bool) -> Expect:
    return Expect(None, {}, (), (), date_dependent=dated)


def test_date_literals_count_only_for_date_dependent_tasks():
    ir = {"nodes": [{"id": "a", "type": "script.run", "params": {"input": {"d": "2026-11-07"}}}]}
    assert date_literal_findings(ir, _expect(False)) == []
    found = date_literal_findings(ir, _expect(True))
    assert [(f.code, f.node_id) for f in found] == [("date_literal", "a")]
    g = grade_static(ir, _expect(True), LintResult(id="x"), recorded.catalog())
    assert g.s2s and "task:date_literal" in g.detectors()
    g = grade_static(ir, _expect(False), LintResult(id="x"), recorded.catalog())
    assert not g.s2s


def test_date_dependent_tasks_are_found_from_run_cases():
    want = {
        "basic-summary-file",  # name_regex の @run_date
        "app11-checkout",  # $run_date
        "app11-return",
        "app01-posting-batch",  # scheduled_at
        "app03-weekly-summary",
        "app03-pair",  # 起動の列の scheduled_at
        "app06-reorder-check",
        "app08-weekly-kpi",
        "app11-overdue",
    }
    got = {t.id for t in recorded.tasks().values() if t.date_dependent}
    assert got == want
    t = recorded.tasks()["app03-pair"]
    assert all(r.expect.date_dependent for r in t.roles)


def test_the_task_flag_overrides_the_run_cases():
    raw = copy.deepcopy(dict(recorded.tasks()["basic-branch-amount"].raw))
    assert not parse_task(raw).date_dependent
    raw["date_dependent"] = True
    assert parse_task(raw).date_dependent
    raw = copy.deepcopy(dict(recorded.tasks()["app11-checkout"].raw))
    raw["date_dependent"] = False
    assert not parse_task(raw).date_dependent
    raw["date_dependent"] = "yes"
    with pytest.raises(ValueError, match="date_dependent"):
        parse_task(raw)


def test_every_oracle_has_no_date_literal():
    """正解の IR はどれも日付を決め打ちしない（誤って捕まえない）。"""
    for t in recorded.tasks().values():
        for role, ir in offline_irs(t).items():
            assert ir_date_literals(ir) == [], (t.id, role)
            assert date_literal_findings(ir, t.role(role).expect) == [], (t.id, role)


def test_the_fixed_date_mutant_is_caught_on_date_dependent_tasks():
    hit = 0
    for t in recorded.tasks().values():
        for role, ir in offline_irs(t).items():
            bad = mutate.fixed_date(copy.deepcopy(ir), t.role(role).expect)
            if not t.date_dependent:
                assert bad is None, t.id
                continue
            if bad is None:
                continue  # 日付の参照が無い（15 分ごとの取り込みなど）
            hit += 1
            found = date_literal_findings(bad, t.role(role).expect)
            assert [f.code for f in found] == ["date_literal"], (t.id, role)
            assert mutate.FIXED_DATE in found[0].detail
    assert hit >= 6


def test_disabling_the_date_check_lets_the_fixed_date_mutant_through(monkeypatch):
    from test_mutate import check_static

    assert not [f for f in check_static() if "mut:fixed_date" in f]
    monkeypatch.setattr(static, "date_literal_findings", lambda *a, **k: [])
    failed = check_static()
    assert failed and all("mut:fixed_date" in f for f in failed), failed


def test_disabling_date_literals_lets_the_fixed_date_mutant_through(monkeypatch):
    from test_mutate import check_static

    monkeypatch.setattr(static, "ir_date_literals", lambda *a, **k: [])
    failed = check_static()
    assert failed and all("mut:fixed_date" in f for f in failed), failed


# --- 予定の週（Mode B） ---


def test_the_window_is_the_scheduled_week_and_the_week_before():
    assert scheduled_window([]) is None
    w = scheduled_window(["2026-11-09T09:00:00+09:00"])
    assert w == (dt.date(2026, 11, 2), dt.date(2026, 11, 15))
    # 日本時間で日付が変わる（UTC では前の日曜）。
    assert scheduled_window(["2026-11-08T15:30:00Z"]) == w
    for day, inside in [
        (dt.datetime(2026, 11, 1, 23, 59, tzinfo=JST), False),
        (dt.datetime(2026, 11, 2, 0, 0, tzinfo=JST), True),
        (dt.datetime(2026, 11, 7, 12, 0, tzinfo=JST), True),
        (dt.datetime(2026, 11, 15, 23, 59, tzinfo=JST), True),
        (dt.datetime(2026, 11, 16, 0, 0, tzinfo=JST), False),
        (dt.datetime(2026, 11, 15, 15, 0, tzinfo=dt.UTC), False),  # 日本時間では 16 日
    ]:
        assert in_window(day, w) is inside, day
    assert not in_window(dt.datetime(2026, 11, 7, tzinfo=JST), None)
    assert dates.week_of(dt.date(2026, 11, 9)) == (dt.date(2026, 11, 9), dt.date(2026, 11, 15))


def test_every_scheduled_task_has_a_window():
    for t in recorded.tasks().values():
        if t.scheduled_at():
            assert scheduled_window(t.scheduled_at()) == (
                dt.date(2026, 11, 2),
                dt.date(2026, 11, 15),
            ), t.id


def test_mode_b_does_not_run_a_scheduled_task_inside_the_window():
    inside = dt.datetime(2026, 11, 10, 1, 0, tzinfo=dt.UTC)  # 日本時間 11-10（予定の週）
    fake = ChatFake()
    ctx = make_ctx(fake, now=inside)
    trial, errors = mode_b.run_trial(
        ctx, TASKS["app03-weekly-summary"], spec("app03-weekly-summary")
    )
    assert trial is None
    assert [e["kind"] for e in errors] == ["date_window"]
    assert "2026-11-02〜2026-11-15" in errors[0]["detail"] and not errors[0].get("stop")
    assert fake.posted == [] and fake.threads == {}  # 何も作らない・話しかけない
    # 予定の課題でなければ同じ日でも流す。
    trial, errors = mode_b.run_trial(
        make_ctx(ChatFake(), now=inside), TASKS["basic-branch-amount"], spec("basic-branch-amount")
    )
    assert trial is not None and errors == []


def test_mode_b_runs_scheduled_tasks_outside_the_window_and_on_the_stub_stack():
    outside = dt.datetime(2026, 11, 16, 1, 0, tzinfo=dt.UTC)
    fake = ChatFake()
    trial, errors = mode_b.run_trial(
        make_ctx(fake, now=outside), TASKS["app03-weekly-summary"], spec("app03-weekly-summary")
    )
    assert trial is not None and errors == [] and fake.posted
    inside = dt.datetime(2026, 11, 10, 1, 0, tzinfo=dt.UTC)
    assert mode_b.date_window_reason(TASKS["app03-weekly-summary"], inside, stub=True) is None
    assert mode_b.date_window_reason(TASKS["app03-weekly-summary"], inside) is not None
    assert mode_b.date_window_reason(TASKS["basic-summary-file"], inside) is None


def test_disabling_the_window_guard_runs_the_task(monkeypatch):
    inside = dt.datetime(2026, 11, 10, 1, 0, tzinfo=dt.UTC)
    monkeypatch.setattr(mode_b, "in_window", lambda *a, **k: False)
    fake = ChatFake()
    trial, errors = mode_b.run_trial(
        make_ctx(fake, now=inside), TASKS["app03-weekly-summary"], spec("app03-weekly-summary")
    )
    assert fake.posted, "守りを外すと流れてしまう（上のテストが守りを見ている）"
