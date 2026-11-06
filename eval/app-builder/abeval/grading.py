"""試行の採点の部品（Mode A・Mode B が共有する・GAP-30 §5）。

- `validate_roles`: その版の `POST /workflows/validate` で S1 を判定する（製品の規則は写さない）。V0 の応答には
  `node_ports` が無いので、出口は ir_lint の結果で補う。
- `static_roles`: ir_lint（HEAD の検証器）と課題の検査で S2s・S2w・S5 を出す（ダミーの UUID にした IR を渡す）。
- `stub_plumbing`: IR を複製して LLM のノードを stub のモデルにして保存し、`stub_ok` の run_case を流す
  （S2d・S3）。終わったら複製と、流して増えたルートのファイルを消す（fixture は残す）。複製の名前には試行ごとの
  短い乱数を足す（中断した実行の複製が残っていても 409 にならない）。
- `run_saved`: 保存されたワークフローそのもので run_case を流す（Mode B の実行・S3・S4）。
- 基盤の失敗（保存できない・時間切れ・5xx など）で流せなかった run_case が 1 つでもあれば、`error` を付けて
  S2d・S3・S4 を None にする（§4.8 の「基盤の失敗は採点しない」。部分だけで採点しない）。呼び出し側は
  `InfraFailure` で試行を errors に回す。
"""

from __future__ import annotations

import datetime as dt
import secrets
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from typing import Any

from abeval import fixtures
from abeval.fixtures import FixtureRecord
from abeval.grade.static import LintCatalog, LintResult, grade_static
from abeval.run_cases import CaseError, Saved, execute_case, grade_case, save_workflows
from abeval.shiki import ShikiClient, ShikiError, StubOnlyViolation
from abeval.tasks import RunCase, Task, stub_cases
from abeval.trial import concrete, offline

Lint = Callable[[list[tuple[str, Mapping]]], dict[str, LintResult]]
INFRA_ERRORS = (CaseError, ShikiError, TimeoutError, StubOnlyViolation, KeyError)


class InfraFailure(RuntimeError):
    """基盤の失敗で採点できない試行（errors に `kind` で入れる・採点しない）。"""

    def __init__(self, kind: str, detail: str, partial: dict | None = None):
        super().__init__(f"{kind}: {detail}")
        self.kind = kind
        self.detail = detail
        self.partial = partial or {}


def raise_if_infra(part: str, result: Mapping) -> None:
    """stub の配管・実行の結果に基盤の失敗があれば InfraFailure（`part` は stub_plumbing か run_case）。"""
    if result.get("error"):
        raise InfraFailure(part, str(result["error"])[:500])


def codes(items: Any) -> list[str]:
    return [str(e.get("code")) for e in items or [] if isinstance(e, Mapping)]


def validate_roles(
    client: ShikiClient, task: Task, rec: FixtureRecord, role_irs: Mapping[str, Mapping]
) -> dict[str, dict]:
    """役割ごとに検証する（置き場所の印は今の fixture の id にする）。"""
    out = {}
    for role, ir in role_irs.items():
        try:
            body = concrete(ir, rec.refs, task)
        except KeyError as e:
            out[role] = {"errors": ["abeval.unknown_placeholder"], "warnings": [], "detail": str(e)}
            continue
        v = client.validate(body)
        out[role] = {
            "errors": codes(v.get("errors")),
            "warnings": codes(v.get("warnings")),
            "node_ports": v.get("node_ports"),
        }
    return out


def s1_of(validation: Mapping[str, Mapping], missing: list[str]) -> bool:
    return not missing and bool(validation) and all(not v["errors"] for v in validation.values())


def static_roles(
    task: Task, role_irs: Mapping[str, Mapping], lint: Lint, catalog: LintCatalog, tag: str
) -> dict[str, Any]:
    """S2s・S2w・S5（役割をまとめた値と、役割ごとの内訳）。"""
    items = [(f"{tag}:{role}", offline(ir, task)) for role, ir in role_irs.items()]
    if not items:
        return {"s2s": None, "s2w": None, "s5": None, "roles": {}, "node_ports": {}}
    results = lint(items)
    roles = {}
    ports = {}
    for role, ir in role_irs.items():
        lr = results[f"{tag}:{role}"]
        g = grade_static(offline(ir, task), task.role(role).expect, lr, catalog)
        roles[role] = g.to_dict()
        ports[role] = {k: list(v) for k, v in lr.node_ports.items()}
    return {
        "s2s": any(r["s2s"] for r in roles.values()),
        "s2w": any(r["s2w"] for r in roles.values()) and not any(r["s2s"] for r in roles.values()),
        "s5": all(r["s5"] for r in roles.values()),
        "roles": roles,
        "node_ports": ports,
    }


@contextmanager
def scratch(client: ShikiClient, rec: FixtureRecord) -> Iterator[list[str]]:
    """この中で記録に足されたワークフローとファイルを、出るときに消して記録から外す（fixture は残す）。"""
    mark = len(rec.created)
    problems: list[str] = []
    try:
        yield problems
    finally:
        added = rec.created[mark:]
        del rec.created[mark:]
        for c in reversed(added):
            try:
                if c.kind == "workflow":
                    client.delete_artifact(c.id, missing_ok=True)
                elif c.kind == "file":
                    client.delete_file(c.id, missing_ok=True)
                else:
                    rec.created.append(c)  # 片付けは試行の最後に回す
            except ShikiError as e:
                problems.append(f"{c.kind} {c.id}: {e}")
                rec.created.append(c)


def _summary(cases: list[dict]) -> dict:
    """run_case の採点をまとめる（基盤の失敗が 1 つでもあれば、どれも None にして `error` を付ける）。"""
    failed = [c for c in cases if "error" in c]
    if failed:
        return {
            "s2d": None,
            "s3": None,
            "s4": None,
            "cases": cases,
            "error": "; ".join(f"{c['case']}: {c['error']}" for c in failed)[:500],
        }
    graded = cases
    return {
        "s2d": any(c["s2d"] for c in graded) if graded else None,
        "s3": all(c["s3"] for c in graded) if graded else None,
        "s4": None
        if not graded or any(c["s4"] is None for c in graded)
        else all(c["s4"] for c in graded),
        "cases": cases,
    }


def _run_cases(
    client: ShikiClient,
    task: Task,
    rec: FixtureRecord,
    cases: list[RunCase],
    wfs: Mapping[str, Saved],
    *,
    stub: bool,
    now: dt.datetime,
    timeout: float,
) -> list[dict]:
    irs = {r: w.ir for r, w in wfs.items()}
    ports = {r: w.node_ports for r, w in wfs.items()}
    out = []
    for case in cases:
        try:
            crec = execute_case(client, task, case, wfs, rec, now=now, timeout=timeout)
        except INFRA_ERRORS as e:
            out.append({"case": case.id, "error": f"{type(e).__name__}: {str(e)[:400]}"})
            continue
        out.append(grade_case(task, case, crec, irs, ports, stub=stub, now=now))
    return out


def stub_plumbing(
    client: ShikiClient,
    task: Task,
    rec: FixtureRecord,
    role_irs: Mapping[str, Mapping],
    *,
    tag: str,
    lint_ports: Mapping[str, Mapping],
    now: dt.datetime,
    timeout: float,
) -> dict:
    """stub の配管（複製を stub のモデルで保存して stub_ok の run_case を流す）。"""
    cases = stub_cases(task)
    if not cases:
        return {
            "s2d": None,
            "s3": None,
            "s4": None,
            "cases": [],
            "skipped": "stub_ok の run_case が無い",
        }
    with scratch(client, rec) as problems:
        try:
            wfs = save_workflows(
                client, task, role_irs, rec, tag=f"{tag}-{secrets.token_hex(2)}", stub=True
            )
        except INFRA_ERRORS as e:
            return {
                "s2d": None,
                "s3": None,
                "s4": None,
                "error": f"{type(e).__name__}: {str(e)[:400]}",
                "cases": [],
            }
        for role, w in wfs.items():
            if not w.node_ports:
                w.node_ports = {k: list(v) for k, v in (lint_ports.get(role) or {}).items()}
        out = _summary(
            _run_cases(client, task, rec, cases, wfs, stub=True, now=now, timeout=timeout)
        )
    if problems:
        out["cleanup_problems"] = problems
    return out


def run_saved(
    client: ShikiClient,
    task: Task,
    rec: FixtureRecord,
    wfs: Mapping[str, Saved],
    *,
    stub: bool,
    now: dt.datetime,
    timeout: float,
) -> dict:
    """保存されたワークフローそのもので run_case を流す（stub では stub_ok だけ）。"""
    cases = stub_cases(task) if stub else list(task.run_cases)
    with scratch(client, rec) as problems:
        out = _summary(
            _run_cases(client, task, rec, cases, wfs, stub=stub, now=now, timeout=timeout)
        )
    if problems:
        out["cleanup_problems"] = problems
    return out


def headline(s1: bool, s2s: bool | None, s2d: bool | None) -> bool | None:
    """Mode A の見出し（正しく保存できる率@1 = S1 ∧ ¬S2s ∧ ¬S2d）。

    S2s・S2d が False と決まったときだけ真にする。保存できて誤りも見つからないが、どちらかが決まらない（stub の
    配管を流さない・stub_ok の run_case が無い）ときは None（率の分母に入れない）。
    """
    if not s1 or s2s or s2d:
        return False
    if s2s is None or s2d is None:
        return None
    return True


def headline_b(s1: bool, s2s: bool | None, s2d: bool | None, s4: bool | None) -> bool | None:
    """Mode B の見出し（使えるワークフロー率 = S1 ∧ ¬S2s ∧ ¬S2d ∧ S4）。決まらない項目があれば None。"""
    a = headline(s1, s2s, s2d)
    if a is False or s4 is False:
        return False
    if a is None or s4 is None:
        return None
    return True


def safe_cleanup(client: ShikiClient, rec: FixtureRecord) -> list[str]:
    """試行の片付け（番兵が無ければ消さずに理由を返す）。誤りの列（空なら片付いた）。"""
    try:
        return fixtures.cleanup(client, rec).errors
    except fixtures.SentinelMissing as e:
        return [f"番兵が無いので片付けません: {e}"]
