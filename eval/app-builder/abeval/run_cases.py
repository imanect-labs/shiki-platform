"""run_case をサーバーで流し、採点の材料（`grade.dynamic.CaseRecord`）を集める（GAP-30 §4.6 の 6・§4.7 の 9）。

正解の IR の結合テスト（tests/test_reference_it.py）・記録の作り直し（scripts/record_fixtures.py）が使い、後の段の
Mode A（stub の配管）と Mode B（実行）も使う。製品の公開の口だけを叩く（`ShikiClient`）。

1. `prepare_ir`: 置き場所の印（`@table:` など）を fixture の id に置き換え、名前を付け替える。`stub=True` では
   LLM のノードの model を stub（`default`・判定は `decide-stub`）にし、effort を外す（stub は受けない）。
2. `save_workflows`: 検証（`/workflows/validate`）して保存する。作った id は fixture の記録に残す（片付けで消える）。
3. `execute_case`: run_case の前に fixture の行を入れ直し、起動の列（`sequence`）を順に流して終わりを待つ。成功した
   step の出力・行・増えたファイルを集める。`no_new_rows` なら最後の起動をもう一度流して行を数え直す。ルートに
   増えたファイルは記録に足す（片付けで消す）。
4. `grade_case`: 動的な検査と照合子をかける（stub で llm_dependent の run_case は照合をせず、動的な検査を最初の
   LLM のノードまでに限る）。「入力が届かない」と「想定外の状態」は stub の run だけで見る（実モデルの出力は
   入力の全文を含まず、状態は照合子の S4 が見る）。
"""

from __future__ import annotations

import copy
import datetime as dt
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from abeval.fixtures import FixtureRecord, reload_rows, resolve_refs, resolve_rel
from abeval.grade.checks import evaluate
from abeval.grade.dynamic import CaseRecord, RunRecord, StepRecord, grade_dynamic
from abeval.grade.static import LLM_NODE_TYPES
from abeval.shiki import ShikiClient, ShikiError
from abeval.tasks import RunCase, Task, substitute

STUB_MODELS = {"llm.invoke": "default", "agent.invoke": "default", "ai.decide": "decide-stub"}


class CaseError(RuntimeError):
    """run_case を流せない（基盤の失敗・採点しない）。"""


def prepare_ir(ir: Mapping, rec: FixtureRecord, task: Task, *, name: str, stub: bool) -> dict:
    out = substitute(copy.deepcopy(dict(ir)), rec.refs, task.fixture_refs())
    out["name"] = name
    if stub:
        for n in out.get("nodes") or []:
            if n.get("type") in LLM_NODE_TYPES:
                params = n.setdefault("params", {})
                params["model"] = STUB_MODELS[n["type"]]
                params.pop("effort", None)
    return out


@dataclass
class Saved:
    id: str
    version: int
    node_ports: dict[str, list[str]]
    ir: dict
    validation: dict


def save_workflows(
    client: ShikiClient,
    task: Task,
    irs: Mapping[str, Mapping],
    rec: FixtureRecord,
    *,
    tag: str,
    stub: bool,
) -> dict[str, Saved]:
    """役割ごとに検証して保存する（名前は `abeval-<tag>-<役割>`）。検証の誤りは CaseError。"""
    out: dict[str, Saved] = {}
    for role, ir in irs.items():
        name = f"abeval-{tag}-{role}"[:64].rstrip("-")
        body = prepare_ir(ir, rec, task, name=name, stub=stub)
        v = client.validate(body)
        if v.get("errors"):
            raise CaseError(f"{task.id}/{role}: 検証を通りません: {v['errors']}")
        try:
            created = client.create_workflow(body)
        except ShikiError as e:
            raise CaseError(f"{task.id}/{role}: 保存できません: {e}") from e
        rec.add("workflow", created["id"], name)
        out[role] = Saved(
            created["id"], created.get("version", 1), v.get("node_ports") or {}, body, v
        )
    return out


def _tables(rec: FixtureRecord, task: Task) -> dict[str, str]:
    return {ref: rec.refs[ref] for ref, kind in task.fixture_refs().items() if kind == "table"}


def snapshot_rows(client: ShikiClient, rec: FixtureRecord, task: Task) -> dict[str, list]:
    return {
        ref: [{"id": r["id"], "data": r.get("data") or {}} for r in client.records(tid)]
        for ref, tid in _tables(rec, task).items()
    }


def _folders(rec: FixtureRecord, task: Task) -> dict[str | None, str | None]:
    """fixture のフォルダーの ref → id（ルートは None → None）。"""
    out: dict[str | None, str | None] = {None: None}
    for ref, kind in task.fixture_refs().items():
        if kind == "folder":
            out[ref] = rec.refs[ref]
    return out


def snapshot_files(client: ShikiClient, rec: FixtureRecord, task: Task) -> set[str]:
    ids: set[str] = set()
    for _, fid in _folders(rec, task).items():
        ids |= {n["id"] for n in client.nodes(fid)}
    return ids


def new_files(client: ShikiClient, rec: FixtureRecord, task: Task, before: set[str]) -> list[dict]:
    out = []
    for ref, fid in _folders(rec, task).items():
        for n in client.nodes(fid):
            if n["id"] in before or n.get("kind") == "folder":
                continue
            try:
                text = client.file_text(n["id"]).get("text")
            except ShikiError as e:
                text = None if e.status in (404, 415, 422) else f"<読めない: {e.status}>"
            out.append({"folder": ref, "id": n["id"], "name": n.get("name"), "text": text})
            if ref is None:
                rec.add("file", n["id"], n.get("name") or "", None, None)
    return out


def _resolve_value(value: Any, rec: FixtureRecord, now: dt.datetime) -> Any:
    return resolve_refs(resolve_rel(value, now), rec.refs, rec.rows)


def _run_once(
    client: ShikiClient,
    wf: Saved,
    role: str | None,
    inp: Mapping,
    *,
    trigger: Mapping | None,
    scheduled_at: str | None,
    timeout: float,
) -> RunRecord:
    try:
        run_id = client.start_run(wf.id, inp, trigger=trigger, scheduled_at=scheduled_at)
    except ShikiError as e:
        if e.status in (400, 422):
            return RunRecord(None, "not_started", dict(inp), [], role, scheduled_at, e.body[:500])
        raise
    if run_id is None:
        return RunRecord(None, "not_started", dict(inp), [], role, scheduled_at, "run_id が null")
    run = client.wait_run(wf.id, run_id, timeout=timeout)
    steps = []
    for s in run.get("steps") or []:
        out = None
        if s.get("status") == "succeeded" and s.get("has_output"):
            out = client.get_step(wf.id, run_id, s["step_path"]).get("output")
        steps.append(
            StepRecord(
                s["step_path"],
                s["node_id"],
                s["status"],
                list(s.get("taken_ports") or []),
                out,
                s.get("error") or None,
            )
        )
    return RunRecord(
        run_id,
        run["status"],
        dict(run.get("input") or inp),
        steps,
        role,
        run.get("scheduled_at"),
        run.get("fail_reason"),
    )


def execute_case(
    client: ShikiClient,
    task: Task,
    case: RunCase,
    wfs: Mapping[str, Saved],
    rec: FixtureRecord,
    *,
    now: dt.datetime,
    timeout: float = 180.0,
    reload: bool = True,
) -> CaseRecord:
    """run_case を流して記録を集める（起動できなかった段は status が not_started）。"""
    if reload and _tables(rec, task):
        reload_rows(client, rec, task.fixtures, now=now)
    rows_before = snapshot_rows(client, rec, task)
    files_before = snapshot_files(client, rec, task)
    runs = []
    last = None
    for step in case.steps:
        role = step.workflow or "main"
        if role not in wfs:
            raise CaseError(f"{task.id}/{case.id}: 役割のワークフローがありません: {role}")
        last = (role, step)
        runs.append(
            _run_once(
                client,
                wfs[role],
                step.workflow,
                _resolve_value(step.input, rec, now),
                trigger=_resolve_value(step.trigger, rec, now) if step.trigger else None,
                scheduled_at=step.scheduled_at,
                timeout=timeout,
            )
        )
    rows = snapshot_rows(client, rec, task)
    files = new_files(client, rec, task, files_before)
    rerun = None
    if case.expect.get("no_new_rows") and last is not None:
        role, step = last
        _run_once(
            client,
            wfs[role],
            step.workflow,
            _resolve_value(step.input, rec, now),
            trigger=_resolve_value(step.trigger, rec, now) if step.trigger else None,
            scheduled_at=step.scheduled_at,
            timeout=timeout,
        )
        rerun = snapshot_rows(client, rec, task)
        new_files(client, rec, task, files_before | {f["id"] for f in files})
    return CaseRecord(runs, rows, rows_before, files, rerun, now.isoformat())


def grade_case(
    task: Task,
    case: RunCase,
    rec: CaseRecord,
    irs: Mapping[str, Mapping],
    node_ports: Mapping[str, Mapping],
    *,
    stub: bool,
    now: dt.datetime,
    force_checks: bool = False,
) -> dict:
    """1 つの run_case の採点（S2d・S3・S4）。`force_checks` は llm_dependent でも照合する（中身の無い IR の確かめ）。"""
    restrict = stub and case.llm_dependent
    expects = {r.role: r.expect for r in task.roles}
    dyn = grade_dynamic(
        irs,
        node_ports,
        expects,
        rec,
        restrict=restrict,
        delivery=stub and case.expect.get("flow_through", True),
        expect_status=case.status if stub and not case.llm_dependent else None,
        skips=case.status == "succeeded",
    )
    checks = None
    if not restrict or force_checks:
        checks = [c.to_dict() for c in evaluate(case.expect, rec.case_result(), task.fixtures, now)]
    failed = [
        {"step": s.step_path, "error": s.error}
        for r in rec.runs
        for s in r.steps
        if s.status == "failed"
    ] + [
        {"run": "not_started", "error": r.fail_reason}
        for r in rec.runs
        if r.status == "not_started"
    ]
    return {
        "case": case.id,
        "s3": dyn.s3,
        "s2d": dyn.s2d,
        "s4": None if checks is None else all(c["ok"] for c in checks),
        "dynamic": dyn.to_dict(),
        "checks": checks,
        "failed_steps": failed,
    }
