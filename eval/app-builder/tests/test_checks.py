"""照合子（equals・contains・regex・in・absent・rows・files・no_new_rows）と正規化。"""

from __future__ import annotations

import datetime as dt
from decimal import Decimal

import pytest

import recorded
from abeval.grade.checks import (
    MISSING,
    CaseResult,
    check_files,
    check_no_new_rows,
    check_pointer,
    check_rows,
    contains,
    equals,
    evaluate,
    expand_expected,
    fixture_value,
    normalize,
    resolve_pointer,
    status_result,
)

NOW = dt.datetime(2026, 11, 9, 0, 0, tzinfo=dt.UTC)


def test_normalize_numbers_and_width():
    assert normalize(4) == normalize(4.0) == normalize("4") == normalize("4.0") == "4"
    assert normalize("4,000") == normalize(4000) == normalize("４０００") == "4000"
    assert normalize(Decimal("150000.00")) == "150000"
    assert normalize(-0.5) == "-0.5" and normalize(0.0) == "0"
    assert normalize("ＡＢＣ　") == "ABC"
    assert normalize(True) is True and normalize(None) is None
    assert normalize({"a": [1.0, "２"]}) == {"a": ["1", "2"]}
    assert normalize("1,23") == "1,23", "桁区切りの形でない数字は文字のまま"


def test_pointer_resolution():
    doc = {"a": [{"b/c": 1, "d~e": 2}], "z": None}
    assert resolve_pointer(doc, "/a/0/b~1c") == 1
    assert resolve_pointer(doc, "/a/0/d~0e") == 2
    assert resolve_pointer(doc, "/a/1") is MISSING
    assert resolve_pointer(doc, "/z") is None
    assert resolve_pointer(doc, "") is doc
    assert resolve_pointer(doc, "a") is MISSING


def test_value_matchers():
    assert equals("８０，０００", 80000), "全角の数字と読点も NFKC で桁区切りの数字になる"
    assert equals("80,000", 80000) and equals(80000.0, "80000")
    assert equals("2026-11-07", {"$regex": r"^\d{4}-\d{2}-\d{2}$"})
    assert not equals(None, {"$regex": "."})
    assert equals(MISSING, {"$absent": True}) and equals(None, {"$absent": True})
    assert not equals("x", {"$absent": True}) and equals("x", {"$absent": False})
    assert not equals(MISSING, None)
    assert contains("件名: 会議室の予約方法", "会議室") and not contains("会議", "会議室")
    assert contains([1, 2.0], "2") and contains([{"a": 1, "b": 2}], {"a": "1"})
    assert contains({"a": 1, "b": 2}, {"b": 2}) and not contains(3, 3)


def test_check_pointer_ops():
    subject = {
        "outputs": {"save": {"name": "要約-2026-11-07.md", "size": 120}},
        "status": "succeeded",
    }

    def ok(op, path, value=None):
        return check_pointer({"op": op, "path": path, "value": value}, subject).ok

    assert ok("equals", "/status", "succeeded")
    assert ok("regex", "/outputs/save/name", r"^要約-\d{4}")
    assert ok("contains", "/outputs/save/name", "要約")
    assert ok("in", "/outputs/save/size", [100, "120"])
    assert ok("absent", "/outputs/missing")
    assert not ok("equals", "/outputs/missing", None)
    assert not ok("regex", "/outputs/missing", ".")
    assert not ok("absent", "/status")


ROWS = {
    "t": [
        {"id": "1", "data": {"rep": "佐藤", "visits": 5, "note": "a"}},
        {"id": "2", "data": {"rep": "鈴木", "visits": 2.0}},
    ]
}


def test_rows_count_where_match_absent_unchanged():
    res = check_rows(
        {
            "table": "t",
            "count": 2,
            "match": [{"rep": "佐藤", "visits": "5"}],
            "absent": [{"rep": "田中"}],
        },
        ROWS,
        ROWS,
    )
    assert all(r.ok for r in res), res
    res = check_rows(
        {"table": "t", "where": {"rep": "鈴木"}, "count": 1, "match": [{"visits": 2}]}, ROWS, {}
    )
    assert all(r.ok for r in res), res
    bad = check_rows(
        {
            "table": "t",
            "where": {"rep": "鈴木"},
            "match": [{"visits": 5}],
            "absent": [{"visits": 2}],
        },
        ROWS,
        {},
    )
    assert [r.ok for r in bad] == [False, False]
    assert check_rows({"table": "t", "unchanged": True}, ROWS, ROWS)[0].ok
    changed = {"t": [ROWS["t"][0], {"id": "2", "data": {"rep": "鈴木", "visits": 3}}]}
    assert not check_rows({"table": "t", "unchanged": True}, changed, ROWS)[0].ok
    assert not check_rows({"table": "nope", "count": 0}, ROWS, {})[0].ok
    absent_field = check_rows(
        {"table": "t", "match": [{"rep": "鈴木", "note": {"$absent": True}}]}, ROWS, {}
    )
    assert absent_field[0].ok


def test_fixture_expressions():
    fx = {
        "tables": [
            {
                "ref": "daily",
                "records": [
                    {"rep": "佐藤", "d": "2026-11-02", "v": 3},
                    {"rep": "佐藤", "d": "2026-11-08", "v": 2},
                    {"rep": "佐藤", "d": "2026-11-09", "v": 7},
                    {"rep": "鈴木", "d": "2026-11-03", "v": 1},
                    {"rep": "田中", "d": "@rel:-1d", "v": 100},
                ],
            }
        ]
    }
    rng = {"field": "d", "from": "2026-11-02", "to": "2026-11-08"}
    assert (
        fixture_value(
            {"table": "daily", "op": "sum", "field": "v", "where": {"rep": "佐藤"}, "range": rng},
            fx,
            NOW,
        )
        == 5
    )
    assert fixture_value({"table": "daily", "op": "count", "range": rng}, fx, NOW) == 4
    # @rel: は fixture を作った時刻で解く（NOW の前日 = 2026-11-08）。
    rel = {"field": "d", "from": "@rel:-1d", "to": "@rel:-1d"}
    assert (
        fixture_value({"table": "daily", "op": "sum", "field": "v", "range": rel}, fx, NOW) == 102
    )
    out = expand_expected(
        {"rows": [{"match": [{"v": {"$fixture": {"table": "daily", "op": "count"}}}]}]}, fx, NOW
    )
    assert out["rows"][0]["match"][0]["v"] == 5
    with pytest.raises(KeyError):
        fixture_value({"table": "none"}, fx, NOW)
    with pytest.raises(ValueError):
        fixture_value({"table": "daily", "op": "avg"}, fx, NOW)


def test_files():
    files = [
        {"folder": "out", "name": "要約-2026-11-07.md", "text": "# 会議室の予約方法\n回答: …"},
        {"folder": None, "name": "要約-2026-11-07.md", "text": "ルート"},
    ]
    spec = {
        "folder": "out",
        "name_regex": r"^要約-\d{4}-\d{2}-\d{2}\.md$",
        "count": 1,
        "contains": ["会議室"],
        "not_contains": ["ルート"],
    }
    assert all(r.ok for r in check_files(spec, files))
    assert not check_files({"folder": "out", "name_regex": "^週報"}, files)[0].ok
    assert not check_files({**spec, "count": 2}, files)[0].ok
    assert not check_files({**spec, "contains": ["無い"]}, files)[1].ok
    assert not check_files({"folder": None, "name_regex": ".", "not_contains": ["ルート"]}, files)[
        1
    ].ok


def test_no_new_rows_and_status():
    after = {"t": [1, 2], "u": [1]}
    assert check_no_new_rows(after, {"t": [1, 2], "u": [1]}).ok
    assert not check_no_new_rows(after, {"t": [1, 2, 3], "u": [1]}).ok
    assert not check_no_new_rows(after, None).ok
    assert status_result("succeeded", ["succeeded", "succeeded"]).ok
    assert not status_result("succeeded", ["succeeded", "failed"]).ok
    assert not status_result("succeeded", []).ok
    assert status_result("failed", ["succeeded", "failed"]).ok
    assert status_result("any", ["cancelled"]).ok


def test_evaluate_on_a_hand_made_result():
    res = CaseResult(["succeeded"], {"outputs": {"a": {"x": 1}}}, ROWS, ROWS, [], {"t": ROWS["t"]})
    exp = {
        "checks": [{"op": "equals", "path": "/outputs/a/x", "value": "1"}],
        "rows": [{"table": "t", "count": 2}],
        "no_new_rows": True,
    }
    assert all(c.ok for c in evaluate(exp, res, {}, NOW))


def _case(task_id: str, case_id: str):
    return next(c for c in recorded.tasks()[task_id].run_cases if c.id == case_id)


# llm_dependent の照合は実モデルで見る（stub の分類は形に合う最小の値なので、ここでは照合しない）。
@pytest.mark.parametrize(
    "key",
    [r for r in recorded.runs() if r[1] == "oracle" and not _case(r[0], r[2]).llm_dependent],
    ids="/".join,
)
def test_recorded_oracle_runs_match_their_expectations(key):
    task = recorded.tasks()[key[0]]
    case = _case(key[0], key[2])
    _, _, rec = recorded.run(*key)
    now = dt.datetime.fromisoformat(rec.now)
    checks = evaluate(case.expect, rec.case_result(), task.fixtures, now)
    assert all(c.ok for c in checks), [c.to_dict() for c in checks if not c.ok]


def test_recorded_null_runs_fail_s4_per_task():
    by_task: dict[str, list[bool]] = {}
    for task_id, variant, case_id in recorded.runs():
        if variant != "null":
            continue
        task = recorded.tasks()[task_id]
        case = next(c for c in task.run_cases if c.id == case_id)
        _, _, rec = recorded.run(task_id, variant, case_id)
        ok = all(
            c.ok
            for c in evaluate(
                case.expect, rec.case_result(), task.fixtures, dt.datetime.fromisoformat(rec.now)
            )
        )
        by_task.setdefault(task_id, []).append(ok)
        if case.expect.get("flow_through", True):
            assert not ok, f"null が {task_id}/{case_id} を通った"
    assert by_task and all(not all(v) for v in by_task.values()), by_task
