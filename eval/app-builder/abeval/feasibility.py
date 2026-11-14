"""版で課題が成り立つか（正解がその版で保存でき、stub の配管で通るか・D-167）。

課題の `requires`（ノードの種類と API の口）だけでは、後から入った設定の項目（保存の `on_name_taken`・「AI に聞く」の
`output_schema`・足す数の `add`・照合の `match_on` など）や実行時の値の取り出し方の差を拾えない。拾えないまま古い版
（V0 = 81a9899b）を測ると、版の差に「機能の差」が混ざる。そこで、版のサーバーで正解の IR を本番の採点と同じ道具
（`grading.stub_plumbing`・出口は ir_lint で補う）で流し、成り立たない課題をその版の対象外にする（費用 0）。

- 正解から入力の並びの注記 `x-order`（D-60・2026-10-26）を外す。知らない版は入力の確かめで起動を断る（fail-closed）が、
  画面に並べる順の注記で、課題の成り立ちには関わらない。
- 正解が保存できない（検証の誤り）・動的な検査で見つかる・照合が落ちる課題は、その版では成り立たない（理由を残す）。
- 時間切れ・サーバーの誤りなど基盤の失敗は、成り立たないとみなさずに止める（誤って対象外にしない）。
- 結果は版の記録の `feasible` に、採点器の版と課題集の sha と一緒に残し、どちらかが変わったら確かめ直す。
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import secrets
from collections.abc import Callable, Mapping
from typing import Any

from abeval import fixtures
from abeval.grade import GRADER_VERSION
from abeval.grade.static import LintCatalog
from abeval.grading import Lint, static_roles, stub_plumbing
from abeval.shiki import ShikiClient
from abeval.tasks import Task


class FeasibilityError(RuntimeError):
    """課題が成り立つかを確かめられない（基盤の失敗・流すのを止める）。"""


def drop_x_order(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {k: drop_x_order(v) for k, v in value.items() if k != "x-order"}
    if isinstance(value, list):
        return [drop_x_order(v) for v in value]
    return value


def tasks_sha(tasks: list[Task]) -> str:
    """課題の正解・fixture・run_case の sha（変われば確かめ直す）。"""
    body = [{"id": t.id, "raw": t.raw} for t in sorted(tasks, key=lambda t: t.id)]
    return hashlib.sha256(json.dumps(body, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


def judge(result: Mapping) -> str | None:
    """stub の配管の結果から、成り立たない理由（成り立てば None）。基盤の失敗は FeasibilityError。"""
    err = result.get("error")
    if err:
        if "検証を通りません" in err:
            return f"正解がこの版で保存できない: {err[:300]}"
        raise FeasibilityError(f"正解を流せません（基盤の失敗）: {err[:300]}")
    if result.get("skipped"):
        return None
    reasons = []
    for case in result.get("cases") or []:
        for f in (case.get("dynamic") or {}).get("findings") or []:
            reasons.append(f"{case.get('case')}: {f.get('code')} {f.get('detail', '')[:120]}")
        for c in case.get("checks") or []:
            if not c.get("ok"):
                reasons.append(
                    f"{case.get('case')}: 照合 {c.get('kind')} {c.get('detail', '')[:120]}"
                )
        for s in case.get("failed_steps") or []:
            e = s.get("error")
            msg = e.get("message") if isinstance(e, Mapping) else str(e)
            reasons.append(
                f"{case.get('case')}: 落ちた step {s.get('node_id') or s.get('run')}: {str(msg)[:120]}"
            )
    if result.get("s2d") or result.get("s4") is False:
        return "正解がこの版で通らない: " + "／".join(reasons[:6])
    return None


def probe(
    client: ShikiClient,
    tasks: list[Task],
    lint: Lint,
    catalog: LintCatalog,
    *,
    now: Callable[[], dt.datetime] = fixtures.now_utc,
    timeout: float = 180.0,
) -> dict[str, str | None]:
    """課題 id → 成り立たない理由（成り立てば None）。"""
    out: dict[str, str | None] = {}
    for task in tasks:
        irs = drop_x_order(task.reference_irs())
        st = static_roles(task, irs, lint, catalog, f"feas-{task.id}"[:12])
        at = now()
        try:
            rec = fixtures.create_fixtures(client, task.fixtures, now=at)
        except fixtures.FixtureError as e:
            raise FeasibilityError(f"{task.id}: fixture を作れません: {e}") from e
        try:
            result = stub_plumbing(
                client,
                task,
                rec,
                irs,
                tag=f"feas-{secrets.token_hex(2)}",
                lint_ports=st["node_ports"],
                now=at,
                timeout=timeout,
            )
        finally:
            report = fixtures.cleanup(client, rec)
        if not report.ok:
            raise FeasibilityError(f"{task.id}: 片付けに失敗しました: {report.errors}")
        try:
            out[task.id] = judge(result)
        except FeasibilityError as e:
            raise FeasibilityError(f"{task.id}: {e}") from e
    return out


def cached(
    record_extra: Mapping, all_tasks: list[Task], want: list[str]
) -> dict[str, str | None] | None:
    """版の記録に残した結果（採点器の版と課題集〔全課題の sha〕が同じで、`want` の課題がすべて入っているときだけ）。"""
    f = record_extra.get("feasible")
    if not isinstance(f, Mapping):
        return None
    if f.get("grader") != GRADER_VERSION or f.get("tasks_sha") != tasks_sha(all_tasks):
        return None
    result = f.get("result")
    if not isinstance(result, Mapping) or any(i not in result for i in want):
        return None
    return {i: result[i] for i in want}


def record_fields(
    record_extra: Mapping, result: Mapping[str, str | None], all_tasks: list[Task]
) -> dict:
    """版の記録に足す項目（同じ採点器の版と課題集の結果には足し、違えば置き換える）。"""
    prev = record_extra.get("feasible")
    same = (
        isinstance(prev, Mapping)
        and prev.get("grader") == GRADER_VERSION
        and prev.get("tasks_sha") == tasks_sha(all_tasks)
        and isinstance(prev.get("result"), Mapping)
    )
    merged = {**(dict(prev["result"]) if same else {}), **dict(result)}
    return {
        "feasible": {
            "grader": GRADER_VERSION,
            "tasks_sha": tasks_sha(all_tasks),
            "result": merged,
        }
    }
