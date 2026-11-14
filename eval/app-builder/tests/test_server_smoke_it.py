"""stub のスタックに対する確かめ（`pytest -m server`・ABEVAL_API_URL が要る・実 LLM は呼ばない）。

例: ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server -q -s

- alice・bob・charlie でログインでき、`/me` の org が合う。`/llm/models` に default がある。カスタム指示が空。
- 番兵を作れる（無ければ作り、このテストで作ったものは最後に消す）。
- fixture（テーブル・行・フォルダー・CSV）を作って片付けると、テーブル・ルート直下のノード・ワークフローの一覧が
  前と同じに戻る。小さな IR の validate、stub の IR の保存・実行・待機・steps・削除が通る。stub のモデルで
  チャットの 1 往復が通る。
- charlie（別テナント）から alice のワークフローとテーブルの id は、無い id と同じ応答（403 か 404）。
- 発話と IR は stub の守り（default・decide-stub だけ）を通す（手元の llm-stub.toml の anthropic は本物の中継を指す）。
"""

from __future__ import annotations

import os
import secrets
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from abeval import fixtures
from abeval.shiki import SEED_ORGS, ShikiClient, ShikiError, StubOnlyViolation

API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")

pytestmark = [
    pytest.mark.server,
    pytest.mark.skipif(
        not API, reason="ABEVAL_API_URL が無いので飛ばします（stub のスタックの URL を渡す）"
    ),
]


def _reachable(url: str) -> bool:
    u = urlsplit(url)
    try:
        with socket.create_connection((u.hostname, u.port or 80), timeout=2):
            return True
    except OSError:
        return False


@pytest.fixture(scope="module")
def users():
    if not _reachable(API):
        pytest.skip(f"{API} に届かないので飛ばします（stub のスタックを起動する）")
    cs = {u: ShikiClient(API, u, stub_only=True, timeout=60.0) for u in SEED_ORGS}
    yield cs
    for c in cs.values():
        c.close()


def test_logins_orgs_models_and_instructions(users):
    for user, org in SEED_ORGS.items():
        me = users[user].login()
        assert me["org"] == org, (user, me)
    alice = users["alice"]
    ids = {m["id"] for m in alice.llm_models()["models"]}
    assert "default" in ids
    ci = alice.custom_instructions()
    assert ci is None or (
        ci["about"].strip() == "" and ci["style"].strip() == "" and ci["tone"] == "default"
    )
    caps = alice.capabilities()
    print(f"\n機能: {caps}")
    assert (
        caps["node_ports"]
        and caps["trigger"]
        and caps["scheduled_at"]
        and caps["custom_instructions"]
    )


def _snapshot(c: ShikiClient) -> dict:
    return {
        "tables": sorted(t["id"] for t in c.tables()),
        "nodes": sorted(n["id"] for n in c.nodes()),
        "workflows": sorted(w["id"] for w in c.workflows()),
    }


STUB_IR = {
    "ir_version": 1,
    "display_name": "評価の確かめ",
    "declared_scopes": [],
    "triggers": [{"kind": "interactive"}],
    "nodes": [
        {
            "id": "ask",
            "type": "llm.invoke",
            "label": "聞く",
            "params": {"model": "default", "prompt": "評価の確かめです"},
        }
    ],
    "edges": [],
}


@pytest.fixture
def sentinel(users):
    """番兵（このテストで作ったものは、落ちても最後に消す）。"""
    alice = users["alice"]
    node, made = fixtures.init_sentinel(alice)
    yield node
    if made:
        alice.delete_folder(node["id"])
        assert fixtures.find_sentinel(alice) is None


def test_fixtures_workflow_chat_and_cleanup(users, sentinel):
    alice, charlie = users["alice"], users["charlie"]
    tag = secrets.token_hex(3)
    before = _snapshot(alice)
    rec = fixtures.new_record("alice")
    spec = {
        "folders": [
            {"ref": "out", "name": f"評価の確かめ-{tag}"},
            {"ref": "sub", "name": "下書き", "parent": "out"},
        ],
        "tables": [
            {
                "ref": "staff",
                "name": f"評価の確かめ-担当者-{tag}",
                "schema": {"fields": [{"key": "name", "display_name": "名前", "type": "text"}]},
                "records": [{"name": "佐藤"}, {"name": "鈴木"}],
            },
            {
                "ref": "daily",
                "name": f"評価の確かめ-日報-{tag}",
                "schema": {
                    "fields": [
                        {"key": "report_date", "display_name": "日付", "type": "date"},
                        {"key": "visits", "display_name": "訪問件数", "type": "number"},
                    ]
                },
                "records": [
                    {"report_date": "@rel:-6d", "visits": 3},
                    {"report_date": "@rel:0d", "visits": 1},
                ],
            },
        ],
        "csv": [
            {
                "ref": "survey",
                "name": "アンケート.csv",
                "folder": "out",
                "text": "点数,感想\n9,よい\n",
            }
        ],
    }
    try:
        now = fixtures.now_utc()
        fixtures.create_fixtures(alice, spec, now=now, record=rec)
        daily = rec.refs["daily"]
        rows = alice.records(daily)
        assert sorted(r["data"]["report_date"] for r in rows) == sorted(
            [fixtures.resolve_rel("@rel:-6d", now), fixtures.resolve_rel("@rel:0d", now)]
        )
        assert alice.file_text(rec.refs["survey"])["text"] == "点数,感想\n9,よい\n"
        assert {n["name"] for n in alice.nodes(rec.refs["out"])} == {"下書き", "アンケート.csv"}

        # 小さな IR の検証（保存はしない）。
        ok = alice.validate({**STUB_IR, "name": f"abeval-smoke-{tag}"})
        assert ok["errors"] == [], ok
        broken = alice.validate(
            {
                **STUB_IR,
                "name": f"abeval-smoke-{tag}",
                "nodes": [{"id": "x", "type": "bogus.node", "params": {}}],
            }
        )
        assert broken["errors"] and all(e["code"] for e in broken["errors"])

        # stub の IR を保存・実行・待機・steps。記録して片付ける。
        wf = alice.create_workflow({**STUB_IR, "name": f"abeval-smoke-{tag}"})
        rec.add("workflow", wf["id"], wf["name"])
        run_id = alice.start_run(wf["id"], {})
        assert run_id
        run = alice.wait_run(wf["id"], run_id, timeout=120)
        assert run["status"] == "succeeded", run
        assert [s["node_id"] for s in run["steps"]] == ["ask"]
        step = alice.get_step(wf["id"], run_id, run["steps"][0]["step_path"])
        assert step["status"] == "succeeded" and "回答" in str(step["output"])
        # 記録していない印付きの複製（試行の開始の後）も片付けで消える。
        dup = alice.create_workflow({**STUB_IR, "name": f"abeval-dup-{tag}"})

        # テナントの越境: charlie から alice のワークフローとテーブルの id は引けず、無い id と同じ応答になる
        # （あるかどうかが漏れない）。ワークフローは artifact の閲覧の認可で 403、テーブルは 404。
        missing = str(uuid.uuid4())
        for get in (charlie.get_workflow, charlie.get_table):
            codes = []
            for target in ((wf["id"] if get == charlie.get_workflow else daily), missing):
                with pytest.raises(ShikiError) as e:
                    get(target)
                codes.append(e.value.status)
            assert codes[0] == codes[1] and codes[0] in (403, 404), (get.__name__, codes)
            print(f"\ncharlie の {get.__name__}: alice の id → {codes[0]}・無い id → {codes[1]}")

        # stub のモデルでチャットの 1 往復（実モデルを指す発話は送る前に止める）。
        th = alice.create_thread()
        rec.add("thread", th["id"], "評価の確かめ")
        with pytest.raises(StubOnlyViolation):
            alice.post_message(th["id"], "やあ", model="claude-haiku-5-5")
        posted = alice.post_message(th["id"], f"評価の確かめ {tag}", model="default")
        events = [e["data"].get("type") for e in alice.stream(th["id"], 0, timeout=120)]
        page = alice.wait_chat_idle(th["id"], timeout=120)
        texts = [
            b.get("text", "")
            for m in page["messages"]
            if m["role"] == "assistant"
            for b in m["content"]
            if b.get("type") == "text"
        ]
        assert any(f"回答: 評価の確かめ {tag}" in t for t in texts), (events, texts)
        assert posted["run_id"]
        print(f"\nSSE の型: {events}")
    finally:
        report = fixtures.cleanup(alice, rec)
        print(f"\n片付け: 消した {len(report.deleted)} 件 {[(k, n) for k, _, n in report.deleted]}")
        assert report.ok, report.errors
    after = _snapshot(alice)
    print(
        f"一覧（前 → 後）: テーブル {len(before['tables'])} → {len(after['tables'])}・"
        f"ルート直下のノード {len(before['nodes'])} → {len(after['nodes'])}・"
        f"ワークフロー {len(before['workflows'])} → {len(after['workflows'])}"
    )
    assert after == before
    assert dup["id"] in {i for _, i, _ in report.deleted}
    with pytest.raises(ShikiError) as e:
        alice.messages(th["id"])
    assert e.value.status == 404


def test_state_flow_fixture_reaches_the_states_and_its_cleanup(users, sentinel):
    """状態の進め方（GAP-113）を製品の公開の口で作る。行は最初の状態から進め方で目当ての状態に届く。

    足したときに最初の状態でない状態を書くと製品は断る（fixture が進め方で進める理由）。定義
    （artifact(kind=fsm)）はテーブルを消しても残るので、片付けで消す。
    """
    alice = users["alice"]
    tag = secrets.token_hex(3)
    before = _snapshot(alice)
    spec = {
        "tables": [
            {
                "ref": "claims",
                "name": f"評価の確かめ-申請-{tag}",
                "schema": {
                    "fields": [
                        {"key": "title", "display_name": "件名", "type": "text"},
                        {
                            "key": "status",
                            "display_name": "状態",
                            "type": "select",
                            "options": ["申請中", "承認済", "計上済"],
                            "indexed": True,
                        },
                    ]
                },
                "records": [
                    {"title": "a", "status": "計上済"},
                    {"title": "b", "status": "申請中"},
                    {"title": "c"},
                ],
                "fsm": {
                    "field": "status",
                    "transitions": [
                        {"from": "申請中", "to": "承認済"},
                        {"from": "承認済", "to": "計上済"},
                    ],
                },
            }
        ]
    }
    rec = fixtures.new_record("alice")
    try:
        fixtures.create_fixtures(alice, spec, record=rec)
        tid, fsm_id = rec.refs["claims"], rec.ids("fsm")[0]
        schema = alice.get_table(tid)["schema"]
        assert schema["status_field"] == "status"
        assert schema["fsm_ref"]["artifact_id"] == fsm_id
        got = {r["data"]["title"]: r["data"].get("status") for r in alice.records(tid)}
        assert got == {"a": "計上済", "b": "申請中", "c": "申請中"}, got
        with pytest.raises(ShikiError) as e:
            alice.create_record(tid, {"title": "d", "status": "承認済"})
        assert e.value.status == 400, e.value
        # 入れ直しも進め方で同じ状態に戻る（run_case の前）。
        fixtures.reload_rows(alice, rec, spec)
        again = {r["data"]["title"]: r["data"].get("status") for r in alice.records(tid)}
        assert again == got
        # 定義はテーブルを消しても残る。
        assert alice.delete_table(tid)
        assert alice.get_fsm(fsm_id)["id"] == fsm_id
        print(f"\nテーブルを消した後も状態の進め方の定義 {fsm_id} は読める（片付けで消す）")
    finally:
        report = fixtures.cleanup(alice, rec)
        assert report.ok, report.errors
    assert ("fsm", fsm_id) in {(k, i) for k, i, _ in report.deleted}
    with pytest.raises(ShikiError) as e:
        alice.get_fsm(fsm_id)
    assert e.value.status == 404
    assert _snapshot(alice) == before
