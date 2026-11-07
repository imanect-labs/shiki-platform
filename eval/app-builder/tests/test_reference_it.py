"""正解の IR（oracle）と中身の無い IR（null）を stub のスタックで流す（`pytest -m server`・ABEVAL_API_URL が要る）。

例: ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server tests/test_reference_it.py -q

課題ごとに:
1. fixture を作る（番兵が無ければ作り、このテストで作った番兵は最後に消す）。
2. 正解の IR の置き場所の印を fixture の id にし、LLM のノードを stub のモデル（default）にして検証・保存する。
3. stub_ok の run_case をすべて流し、動的な検査（dead edge・差し込みの残り・入力が届かない）がゼロで、照合が全部
   通ることを確かめる（llm_dependent の run_case は照合をせず、動的な検査を最初の LLM のノードまでに限る）。
4. 中身の無い IR（stub の `emitwf:ok` と同じ形）で同じ run_case を流し、照合（llm_dependent も含める）が落ちる
   ことを確かめる。S4 は試行の単位（全 run_case の照合が通る）なので、課題の S4 が落ちることと、何かを変える
   run_case がどれも落ちることを見る（何も変えないのが正解の run_case〔`expect.flow_through: false`〕は null も通る）。
5. 片付けて、テーブル・ルート直下のノード・ワークフローの一覧が前と同じに戻り、状態の進め方の定義も消えたことを
   確かめる。

実 LLM は呼ばない（`ShikiClient(stub_only=True)` が実モデルを指す IR を送る前に止める）。
"""

from __future__ import annotations

import json
import os
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from abeval import fixtures
from abeval.grade.mutate import null_ir
from abeval.run_cases import Saved, execute_case, grade_case, save_workflows
from abeval.shiki import ShikiClient, ShikiError
from abeval.tasks import load_tasks, stub_cases

API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")
TASKS = load_tasks()

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
def alice():
    if not _reachable(API):
        pytest.skip(f"{API} に届かないので飛ばします（stub のスタックを起動する）")
    c = ShikiClient(API, "alice", stub_only=True, timeout=60.0)
    c.login()
    node, made = fixtures.init_sentinel(c)
    yield c
    if made:
        c.delete_folder(node["id"], missing_ok=True)
    c.close()


def _snapshot(c: ShikiClient) -> dict:
    return {
        "tables": sorted(t["id"] for t in c.tables()),
        "nodes": sorted(n["id"] for n in c.nodes() if n.get("name") != fixtures.SENTINEL_NAME),
        "workflows": sorted(w["id"] for w in c.workflows()),
    }


def _save_null(c: ShikiClient, task, rec, tag: str) -> dict[str, Saved]:
    out = {}
    for role in task.reference_irs():
        name = f"abeval-{tag}-null-{role}"[:64].rstrip("-")
        ir = null_ir(name)
        created = c.create_workflow(ir)
        rec.add("workflow", created["id"], name)
        out[role] = Saved(created["id"], 1, {"compute": ["out"]}, ir, {})
    return out


@pytest.mark.parametrize("task", TASKS, ids=[t.id for t in TASKS])
def test_reference_passes_and_null_fails(alice, task):
    before = _snapshot(alice)
    now = fixtures.now_utc()
    rec = fixtures.create_fixtures(alice, task.fixtures, now=now)
    problems: list[str] = []
    try:
        tag = f"ref-{uuid.uuid4().hex[:6]}"
        wfs = save_workflows(alice, task, task.reference_irs(), rec, tag=tag, stub=True)
        irs = {r: w.ir for r, w in wfs.items()}
        ports = {r: w.node_ports for r, w in wfs.items()}
        cases = stub_cases(task)
        assert cases, f"{task.id}: stub_ok の run_case がありません"
        for case in cases:
            crec = execute_case(alice, task, case, wfs, rec, now=now)
            g = grade_case(task, case, crec, irs, ports, stub=True, now=now)
            print(f"\n{task.id}/{case.id}: s3={g['s3']} s2d={g['s2d']} s4={g['s4']}")
            if g["s2d"] or g["s4"] is False or (g["s4"] is None and not case.llm_dependent):
                problems.append(f"oracle {case.id}: {json.dumps(g, ensure_ascii=False)[:3000]}")
        nulls = _save_null(alice, task, rec, tag)
        null_irs = {r: w.ir for r, w in nulls.items()}
        null_ports = {r: w.node_ports for r, w in nulls.items()}
        null_s4 = []
        for case in cases:
            crec = execute_case(alice, task, case, nulls, rec, now=now)
            g = grade_case(
                task, case, crec, null_irs, null_ports, stub=True, now=now, force_checks=True
            )
            null_s4.append(g["s4"])
            if g["s4"] and case.expect.get("flow_through", True):
                problems.append(
                    f"null が {case.id}（何かを変える run_case）の照合を通った: {json.dumps(g, ensure_ascii=False)[:2000]}"
                )
        # S4 は試行の単位（全 run_case の照合が通る）。何も変えないのが正解の run_case は null も通る。
        if all(null_s4):
            problems.append(f"null が S4 を通った（{null_s4}）")
    finally:
        report = fixtures.cleanup(alice, rec)
    assert report.ok, report.errors
    assert not problems, "\n".join(problems)
    assert _snapshot(alice) == before, "片付けの後に残ったものがあります"
    # 状態の進め方の定義はテーブルを消しても残るので、片付けが消したことを確かめる（GAP-113）。
    for fid in rec.ids("fsm"):
        with pytest.raises(ShikiError) as e:
            alice.get_fsm(fid)
        assert e.value.status == 404, fid
