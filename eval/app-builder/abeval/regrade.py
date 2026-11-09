"""採点し直し（`python -m abeval grade`・GAP-30 §5）。結果（`results/<exp>.json`）の試行の IR を今の採点器で採点し直す。

- 静的（既定・サーバー不要）: ir_lint（HEAD）と課題の検査で S2s・S2w・S5 を出し直す。結果の採点の規則の版
  （`grader.grader`）が今の GRADER_VERSION と違えば止める（動的な検査の規則も変わり得るので、S2d が前の規則のまま
  新しい版の印だけが付く。`--stub-runs` で出し直す）。
- `--stub-runs`: 静的な採点に加え、試行の IR（保存された最後の IR・fixture の id は印に戻したもの。生成と会話の
  キャッシュの最後の IR と同じもの）を、その版のサーバー（`serve --label <版>`・:10067）で stub の配管に流し直して
  S2d を出し直す（BUG-267・268 の直しを前の結果に当てる）。
  - Mode A: S2d・S3 を stub の配管で出し直す。S1 はその版の検証で決まり、採点器に依らないので残す。
  - Mode B: stub の配管を流し直し、S2d は「実モデルの run_case の S2d（残す）∨ stub の配管の S2d」。S3・S4 と実モデルの
    run_case の記録（`runs`）は会話を流し直さないと出せないので、前の採点の値を残す。残したものは試行の
    `regraded.kept` に、前の採点器の版は `regraded.from_grader` に書く（結果を読む人が分かるように）。
  - 費用 0（LLM を呼ばない）: stub の配管は LLM のノードを stub のモデル（`default`・`decide-stub`）にして保存する。
    流す前に、版のサーバーの点検（`doctor.version_guard`。既定のプロバイダが stub で、anthropic が abeval.proxy を
    指し鍵がダミー）と振る舞いの指紋を確かめる。abeval.proxy に実行を登録しないので、もし要求が届いても上流へは
    送らずに断る（止めていれば繋がらない）。
  - 版のサーバーは試行の版（`label`）と同じものだけ。実験に版が 2 つ以上あれば止める。
  - 基盤の失敗（保存・実行・時間切れ・fixture）で流せなかった試行が 1 つでもあれば、何も書かずに止める（部分だけ
    新しい採点器で採点した結果を残さない）。

`run --go` の採点し直し（Mode A は生成のキャッシュから採点する）を広げず、こちらに置いた理由: `run --go` は取り込み
（abeval.proxy に capture_only の実行を登録する）と計画を通り、Mode B は会話から流し直す（キャッシュのキーに採点器の
版が入る）。採点し直しに要るのは、結果にある IR と版のサーバーだけなので、abeval.proxy にも生成のキャッシュにも頼らない
道にした（結果をコミットした別の checkout でも同じに流せる）。
"""

from __future__ import annotations

import datetime as dt
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import Any

from abeval import fixtures
from abeval.fixtures import FixtureError
from abeval.grade import GRADER_VERSION
from abeval.grade.static import LintCatalog, grader_identity
from abeval.grading import (
    InfraFailure,
    Lint,
    headline,
    headline_b,
    raise_if_infra,
    safe_cleanup,
    static_roles,
    stub_plumbing,
)
from abeval.shiki import ShikiClient, ShikiError
from abeval.tasks import Task

# Mode B で前の採点の値を残すもの（会話を流し直さないと出せない）。
MODE_B_KEPT = ("s1", "s3", "s4", "runs")
MODE_B_KEPT_NOTE = (
    "S3・S4 と実モデルの run_case の記録（runs・その S2d）は、会話を流し直さないと出せないので、"
    "前の採点の値を残した"
)


class RegradeError(RuntimeError):
    """採点し直せない（何も書かない）。"""


@dataclass
class Outcome:
    trials: int = 0
    stub_trials: int = 0
    # 見出しが変わった試行（trial_id, 前, 後, 新しく見つかった動的な検査のコード）。
    changed: list[tuple[str, Any, Any, list[str]]] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)


def gradable(t: Mapping) -> bool:
    """採点し直す試行（IR がある。Mode A は emit の試行だけ・`mode_a.grade` と同じ）。"""
    if not (t.get("irs") or {}):
        return False
    return t.get("mode") == "b" or t.get("status") == "emit"


def check_rules(doc: Mapping, *, stub_runs: bool) -> None:
    """静的だけの採点し直しで、採点の規則の版が結果と違えば止める（S2d が前の規則のまま新しい版の印が付くため）。"""
    prev = (doc.get("grader") or {}).get("grader")
    if not stub_runs and prev != GRADER_VERSION:
        raise RegradeError(
            f"結果の採点の規則の版は {prev}、いまは {GRADER_VERSION} です。動的な検査（S2d）の規則も変わり得るので、"
            "静的な層だけでは採点し直しません。版のサーバーを serve で起動し、--stub-runs を付けて S2d も出し直します"
        )


def stub_label(doc: Mapping) -> str:
    """stub の配管を流し直す試行の版（1 つでなければ止める）。"""
    labels = sorted({str(t.get("label")) for t in doc.get("trials") or [] if _stub_target(t)})
    if len(labels) != 1:
        raise RegradeError(
            f"stub の配管を流し直す試行の版が 1 つではありません: {labels}（実験を版ごとに分けて流します）"
        )
    return labels[0]


def _stub_target(t: Mapping) -> bool:
    """stub の配管を流し直す試行（保存できて、前の採点でも stub の配管を流したもの）。"""
    return gradable(t) and bool(t.get("s1")) and "stub" in t


def _new_codes(old: Mapping | None, new: Mapping | None) -> list[str]:
    def codes(part: Mapping | None) -> list[str]:
        return [
            f["code"]
            for c in (part or {}).get("cases") or []
            for f in (c.get("dynamic") or {}).get("findings") or []
        ]

    before = codes(old)
    after = codes(new)
    for c in before:
        if c in after:
            after.remove(c)
    return after


def _restatic(t: dict, task: Task, lint: Lint, catalog: LintCatalog) -> dict:
    st = static_roles(task, t["irs"], lint, catalog, str(t["key"])[:12])
    t["static"] = {k: v for k, v in st.items() if k != "node_ports"}
    t["s2s"], t["s2w"], t["s5"] = st["s2s"], st["s2w"], st["s5"]
    return st


def _rehead(t: dict) -> None:
    if t.get("mode") == "b":
        t["headline"] = headline_b(t.get("s1"), t.get("s2s"), t.get("s2d"), t.get("s4"))
    else:
        t["headline"] = headline(t.get("s1"), t.get("s2s"), t.get("s2d"))


def _restub(
    t: dict,
    task: Task,
    client: ShikiClient,
    rec: fixtures.FixtureRecord,
    st: Mapping,
    *,
    now: dt.datetime,
    timeout: float,
) -> None:
    """stub の配管を流し直して S2d（Mode A は S3 も）を出し直す（基盤の失敗は InfraFailure）。"""
    old = t.get("stub")
    stub = stub_plumbing(
        client,
        task,
        rec,
        t["irs"],
        tag=str(t["key"])[:8],
        lint_ports=st["node_ports"],
        now=now,
        timeout=timeout,
    )
    raise_if_infra("stub_plumbing", stub)
    t["stub"] = stub
    if t.get("mode") == "b":
        runs = (t.get("runs") or {}).get("s2d")
        t["s2d"] = runs if stub.get("s2d") is None else bool(runs) or stub["s2d"]
    else:
        t["s2d"], t["s3"] = stub.get("s2d"), stub.get("s3")
    t["_new_codes"] = _new_codes(old, stub)


def regrade(
    doc: dict,
    tasks: Mapping[str, Task],
    lint: Lint,
    catalog: LintCatalog,
    *,
    client: ShikiClient | None = None,
    now: Callable[[], dt.datetime] = fixtures.now_utc,
    timeout: float = 180.0,
) -> Outcome:
    """`doc` の試行をその場で採点し直す（`client` があれば stub の配管も）。基盤の失敗は `Outcome.errors` に入れる。

    呼び出し側は、errors が空のときだけ結果を書く。
    """
    out = Outcome()
    prev = grader_identity(doc.get("grader"))
    by_task: dict[str, list[dict]] = {}
    for t in doc.get("trials") or []:
        if gradable(t) and t.get("task") in tasks:
            by_task.setdefault(str(t["task"]), []).append(t)
    for task_id, group in by_task.items():
        task = tasks[task_id]
        statics = {id(t): _restatic(t, task, lint, catalog) for t in group}
        out.trials += len(group)
        targets = [t for t in group if client is not None and _stub_target(t)]
        if targets and client is not None:
            _restub_task(task, targets, statics, client, out, now=now, timeout=timeout)
        stubbed = {id(t) for t in targets}
        for t in group:
            before = t.get("headline")
            _rehead(t)
            codes = t.pop("_new_codes", [])
            if t.get("headline") != before:
                out.changed.append((str(t.get("trial_id")), before, t.get("headline"), codes))
            t["regraded"] = _mark(t, prev, stubbed=id(t) in stubbed)
    return out


def _mark(t: Mapping, prev: Mapping, *, stubbed: bool) -> dict:
    """試行に残す採点し直しの印（出し直したもの・前の採点の値を残したもの）。"""
    mode_b = t.get("mode") == "b"
    redone = ["s2s", "s2w", "s5"]
    if stubbed:
        redone += ["stub", "s2d"] if mode_b else ["stub", "s2d", "s3"]
    mark: dict = {
        "grader": GRADER_VERSION,
        "from_grader": prev,
        "redone": redone,
        "kept": list(MODE_B_KEPT) if mode_b else ["s1"],
    }
    if mode_b:
        mark["note"] = MODE_B_KEPT_NOTE
    if not stubbed:
        mark["note_s2d"] = "S2d は前の採点のまま（stub の配管を流し直していない）"
    return mark


def _restub_task(
    task: Task,
    targets: list[dict],
    statics: Mapping[int, Mapping],
    client: ShikiClient,
    out: Outcome,
    *,
    now: Callable[[], dt.datetime],
    timeout: float,
) -> None:
    """1 課題の fixture を作り、その課題の試行の stub の配管を順に流す（終わったら片付ける）。"""
    try:
        rec = fixtures.create_fixtures(client, task.fixtures, now=now())
    except (FixtureError, fixtures.SentinelMissing) as e:
        out.errors += [f"{t.get('trial_id')}: fixture: {str(e)[:300]}" for t in targets]
        if getattr(e, "record", None) is not None:
            safe_cleanup(client, e.record)
        return
    try:
        for t in targets:
            try:
                _restub(t, task, client, rec, statics[id(t)], now=now(), timeout=timeout)
            except InfraFailure as e:
                out.errors.append(f"{t.get('trial_id')}: {e.kind}: {e.detail[:300]}")
            except (ShikiError, TimeoutError) as e:
                out.errors.append(f"{t.get('trial_id')}: {type(e).__name__}: {str(e)[:300]}")
            else:
                out.stub_trials += 1
    finally:
        problems = safe_cleanup(client, rec)
        if problems:
            out.errors.append(f"{task.id}: 片付け: {'; '.join(problems)[:300]}")


def regrade_note(doc: Mapping, out: Outcome, *, label: str | None, at: dt.datetime) -> dict:
    """結果に残す採点し直しの記録（`regrades` に足す）。"""
    return {
        "at": at.isoformat(timespec="seconds"),
        "grader": GRADER_VERSION,
        "from_grader": grader_identity(doc.get("grader")),
        "stub_runs": label is not None,
        "label": label,
        "trials": out.trials,
        "stub_trials": out.stub_trials,
        "changed_headlines": len(out.changed),
    }
