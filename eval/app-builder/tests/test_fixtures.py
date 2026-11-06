"""abeval.fixtures（@rel: の解決・作った分だけの片付け・番兵）。偽の shiki（MockTransport）で確かめる。"""

from __future__ import annotations

import datetime as dt

import pytest

from abeval import fixtures
from abeval.fixtures import FixtureError, FixtureRecord, SentinelMissing
from abeval.shiki import ShikiClient
from fake_shiki import FakeShiki

API = "http://localhost:8080"
UTC = dt.UTC


def at(s: str) -> dt.datetime:
    return dt.datetime.fromisoformat(s)


# --- @rel: ---


@pytest.mark.parametrize(
    ("now", "value", "want"),
    [
        # JST では 11/06 00:30（UTC では 11/05）。日付は日本時間で数える。
        ("2026-11-05T15:30:00+00:00", "@rel:0d", "2026-11-06"),
        ("2026-11-05T15:30:00+00:00", "@rel:-1d", "2026-11-05"),
        ("2026-11-05T15:30:00+00:00", "@rel:-6d", "2026-10-31"),
        ("2026-11-05T15:30:00+00:00", "@rel:+1w", "2026-11-13"),
        ("2026-11-05T15:30:00+00:00", "@rel:-1w", "2026-10-30"),
        # JST の 23:59（まだ 11/05）。
        ("2026-11-05T14:59:00+00:00", "@rel:0d", "2026-11-05"),
        ("2026-11-05T15:00:00+00:00", "@rel:0d", "2026-11-06"),
        ("2026-11-05T15:30:00+00:00", "@rel:-6dT09:00", "2026-10-31T09:00:00+09:00"),
        ("2026-11-05T15:30:45+00:00", "@rel:+2h", "2026-11-06T02:30:00+09:00"),
        ("2026-11-05T15:30:45+00:00", "@rel:-90m", "2026-11-05T23:00:00+09:00"),
    ],
)
def test_rel_dates_are_jst(now, value, want):
    assert fixtures.resolve_rel(value, at(now)) == want


def test_rel_resolves_nested_values_and_leaves_others():
    now = at("2026-11-05T15:30:00+00:00")
    got = fixtures.resolve_rel(
        {"a": "@rel:-1d", "b": ["x", "@rel:0d", 3], "c": {"d": "@relx"}, "e": None}, now
    )
    assert got == {"a": "2026-11-05", "b": ["x", "2026-11-06", 3], "c": {"d": "@relx"}, "e": None}


@pytest.mark.parametrize(
    "value", ["@rel:", "@rel:x", "@rel:-1y", "@rel:-1hT09:00", "@rel:0dT25:00", "@rel:0dT9:00"]
)
def test_bad_rel_is_an_error(value):
    with pytest.raises(FixtureError):
        fixtures.resolve_rel(value, at("2026-11-05T15:30:00+00:00"))


def test_rel_needs_an_aware_now():
    with pytest.raises(ValueError):
        fixtures.resolve_rel("@rel:0d", dt.datetime(2026, 11, 5))


def test_refs():
    refs, rows = {"t": "T-1"}, {"t": ["R-0", "R-1"]}
    assert fixtures.resolve_refs({"x": "@id:t", "y": ["@record:t:1"]}, refs, rows) == {
        "x": "T-1",
        "y": ["R-1"],
    }
    for bad in ("@id:nope", "@record:t:2", "@record:u:0"):
        with pytest.raises(FixtureError):
            fixtures.resolve_refs(bad, refs, rows)


# --- 作る・片付ける ---


@pytest.fixture
def fake():
    return FakeShiki()


@pytest.fixture
def alice(fake):
    c = ShikiClient(API, "alice", transport=fake.transport())
    yield c
    c.close()


SPEC = {
    "folders": [
        {"ref": "weekly", "name": "週報"},
        {"ref": "sub", "name": "下書き", "parent": "weekly"},
    ],
    "tables": [
        {
            "ref": "staff",
            "name": "担当者",
            "schema": {"fields": [{"key": "name", "display_name": "名前", "type": "text"}]},
            "records": [{"name": "佐藤"}, {"name": "鈴木"}],
        },
        {
            "ref": "daily",
            "name": "日報",
            "schema": {
                "fields": [
                    {"key": "report_date", "display_name": "日付", "type": "date"},
                    {
                        "key": "staff",
                        "display_name": "担当",
                        "type": "record_ref",
                        "ref_table": "@id:staff",
                    },
                ]
            },
            "records": [{"report_date": "@rel:-6d", "staff": "@record:staff:1"}],
        },
    ],
    "csv": [{"ref": "survey", "name": "アンケート.csv", "folder": "weekly", "text": "a,b\n1,2\n"}],
}


def test_creating_refuses_without_the_sentinel(fake, alice):
    with pytest.raises(SentinelMissing):
        fixtures.create_fixtures(alice, SPEC)
    assert not fake.nodes and not fake.tables
    assert not fake.requests_to("/folders", "POST")


def test_create_records_ids_in_order(fake, alice):
    fixtures.init_sentinel(alice)
    now = at("2026-11-05T15:30:00+00:00")
    rec = fixtures.create_fixtures(alice, SPEC, now=now)
    kinds = [(c.kind, c.name) for c in rec.created]
    assert kinds == [
        ("folder", "週報"),
        ("folder", "下書き"),
        ("table", "担当者"),
        ("table", "日報"),
        ("file", "アンケート.csv"),
    ]
    weekly, sub = rec.refs["weekly"], rec.refs["sub"]
    assert fake.nodes[sub]["parent_id"] == weekly
    assert fake.nodes[rec.refs["survey"]]["parent_id"] == weekly
    assert fake.nodes[rec.refs["survey"]]["text"] == "a,b\n1,2\n"
    daily = fake.tables[rec.refs["daily"]]
    assert daily["schema"]["fields"][1]["ref_table"] == rec.refs["staff"]
    rows = [r for r in fake.records.values() if r["table_id"] == daily["id"]]
    assert rows[0]["data"] == {"report_date": "2026-10-31", "staff": rec.rows["staff"][1]}
    assert len(rec.rows["staff"]) == 2
    back = FixtureRecord.from_dict(rec.to_dict())
    assert back.to_dict() == rec.to_dict()


def _make_world(alice):
    """試行の前からあるもの（本人の資源・前の印付き）を作る。"""
    fixtures.init_sentinel(alice)
    keep = {
        "folder": alice.create_folder("既存のフォルダー")["id"],
        "table": alice.create_table("既存のテーブル", {"fields": []})["id"],
        "workflow": alice.create_workflow({"name": "user-flow", "nodes": []})["id"],
        "old-marked": alice.create_workflow({"name": "abeval-old", "nodes": []})["id"],
        "old-marked-folder": alice.create_folder("abeval-old-folder")["id"],
    }
    return keep


def test_cleanup_removes_only_recorded_and_marked_after_start(fake, alice):
    keep = _make_world(alice)
    sentinel = fixtures.find_sentinel(alice)["id"]
    rec = fixtures.create_fixtures(alice, SPEC)
    # 試行の間に作られたもの: 記録したもの（スレッド）と、印付き（複製したワークフロー・テーブル・フォルダー）と、
    # 印の無いもの（本人が作った・記録していない）。
    th = alice.create_thread()["id"]
    rec.add("thread", th, "スレッド")
    dup = alice.create_workflow({"name": "abeval-dup-1234", "nodes": []})["id"]
    tmp_table = alice.create_table("abeval-tmp", {"fields": []})["id"]
    tmp_folder = alice.create_folder("abeval-run-x")["id"]
    unmarked = alice.create_workflow({"name": "made-by-user-meanwhile", "nodes": []})["id"]
    report = fixtures.cleanup(alice, rec)
    assert report.ok, report.errors
    gone = {i for _, i, _ in report.deleted}
    assert gone == {
        *rec.ids("folder"),
        *rec.ids("table"),
        *rec.ids("file"),
        th,
        dup,
        tmp_table,
        tmp_folder,
    }
    for k in ("folder", "old-marked-folder"):
        assert keep[k] in fake.nodes
    assert keep["table"] in fake.tables
    for k in ("workflow", "old-marked"):
        assert keep[k] in fake.workflows
    assert unmarked in fake.workflows
    assert sentinel in fake.nodes
    assert th not in fake.threads and dup not in fake.workflows
    # 行はテーブルごと消える（行を 1 件ずつ消しに行かない）。
    assert not any("/records/" in r.url.path for r in fake.seen if r.method == "DELETE")


def test_cleanup_twice_reports_missing(fake, alice):
    fixtures.init_sentinel(alice)
    rec = fixtures.create_fixtures(alice, SPEC)
    fixtures.cleanup(alice, rec)
    again = fixtures.cleanup(alice, rec)
    assert again.ok and not again.deleted
    assert {i for _, i in again.missing} == {c.id for c in rec.created}


def test_cleanup_refuses_without_the_sentinel(fake, alice):
    fixtures.init_sentinel(alice)
    rec = fixtures.create_fixtures(alice, SPEC)
    alice.delete_folder(fixtures.find_sentinel(alice)["id"])
    before = (dict(fake.nodes), dict(fake.tables))
    with pytest.raises(SentinelMissing):
        fixtures.cleanup(alice, rec)
    assert (fake.nodes, fake.tables) == before
    assert not [r for r in fake.seen if r.method == "DELETE"][1:]


def test_partial_failure_keeps_the_record_for_cleanup(fake, alice):
    fixtures.init_sentinel(alice)
    alice.create_table("日報", {"fields": []})  # 同じ名前（409 で 2 つ目のテーブルの作成が落ちる）
    with pytest.raises(FixtureError) as e:
        fixtures.create_fixtures(alice, SPEC)
    rec = e.value.record
    assert [c.name for c in rec.created] == ["週報", "下書き", "担当者"]
    report = fixtures.cleanup(alice, rec)
    assert report.ok
    assert {n["name"] for n in fake.nodes.values()} == {fixtures.SENTINEL_NAME}
    assert {t["name"] for t in fake.tables.values()} == {"日報"}


def test_bad_specs(fake, alice):
    fixtures.init_sentinel(alice)
    for spec in (
        {"folders": [{"ref": "a", "name": "x", "parent": "nope"}]},
        {"tables": [{"ref": "t"}]},
        {"csv": [{"name": "a.csv"}]},
        {"tables": "x"},
        {"tables": [{"name": "t", "records": [{"d": "@rel:bad"}]}]},
    ):
        with pytest.raises(FixtureError) as e:
            fixtures.create_fixtures(alice, spec)
        fixtures.cleanup(alice, e.value.record)
    assert {n["name"] for n in fake.nodes.values()} == {fixtures.SENTINEL_NAME}
    assert not fake.tables


def test_reload_rows(fake, alice):
    fixtures.init_sentinel(alice)
    rec = fixtures.create_fixtures(alice, SPEC)
    staff = rec.refs["staff"]
    first = list(rec.rows["staff"])
    # 実行が行を書き足した・書き換えたあとで入れ直す。
    alice.create_record(staff, {"name": "増えた行"})
    fixtures.reload_rows(alice, rec, {"tables": SPEC["tables"][:1]})
    names = sorted(r["data"]["name"] for r in fake.records.values() if r["table_id"] == staff)
    assert names == ["佐藤", "鈴木"]
    assert rec.rows["staff"] != first and len(rec.rows["staff"]) == 2
    with pytest.raises(FixtureError, match="記録に無い"):
        fixtures.reload_rows(alice, rec, {"tables": [{"ref": "other", "name": "x"}]})


def test_recorded_row_in_a_table_we_did_not_create(fake, alice):
    fixtures.init_sentinel(alice)
    table = alice.create_table("本人のテーブル", {"fields": []})["id"]
    rec = fixtures.new_record("alice")
    row = alice.create_record(table, {"x": 1})["id"]
    rec.add("record", row, "行", parent=table)
    report = fixtures.cleanup(alice, rec)
    assert ("record", row, "行") in report.deleted
    assert table in fake.tables and row not in fake.records


def test_sentinel_is_created_once(fake, alice):
    node, made = fixtures.init_sentinel(alice)
    assert made
    again, made2 = fixtures.init_sentinel(alice)
    assert not made2 and again["id"] == node["id"]
    with pytest.raises(ValueError):
        fixtures.FixtureRecord("alice", "x").add("bogus", "1", "n")
