"""採点器のオフラインのテストが読む記録を作り直す（tests/fixtures/）。

    uv run python scripts/record_fixtures.py --lint                                   # ir_lint の出力（バイナリが要る）
    uv run python scripts/record_fixtures.py --runs --api-url http://localhost:8080   # run の記録（stub のスタックが要る）

- `--lint`: `tests/fixtures/ir_lint/catalog.json`（`ir_lint --catalog`）と `tests/fixtures/ir_lint/core.jsonl`
  （中核の課題の正解の IR・その壊し方 8 通り・中身の無い IR を ir_lint に掛けた結果）。置き場所の印はダミーの
  UUID にする（`tasks.offline_irs`）。各行に入力の IR の sha（`ir_sha`）を足すので、課題を変えて記録し直して
  いなければオフラインのテストが気づく。実物との一致は `pytest -m lint_bin`（tests/test_lint_bin.py）が見る。
- `--runs`: [`RUN_TASKS`] の課題を stub のスタックで流した記録（`tests/fixtures/runs/<課題>.<種類>.<run_case>.json`）。
  種類は oracle（正解）・null（中身の無い IR）・壊し方の名前（V0 の検証でも保存できる escape_var・input_key）。
  stub の守り（`ShikiClient(stub_only=True)`）を通し、実 LLM は呼ばない。fixture と保存したワークフローは消す。
"""

from __future__ import annotations

import argparse
import json
import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from abeval import fixtures  # noqa: E402
from abeval.config import ROOT, canonical_json, sha256_hex  # noqa: E402
from abeval.gen_checks import lint_corpus  # noqa: E402
from abeval.grade import mutate  # noqa: E402
from abeval.grade.static import catalog_from_bin, find_lint_bin, run_ir_lint  # noqa: E402
from abeval.run_cases import Saved, execute_case, prepare_ir, save_workflows  # noqa: E402
from abeval.shiki import ShikiClient  # noqa: E402
from abeval.tasks import load_tasks, stub_cases  # noqa: E402

FIXTURES = ROOT / "tests" / "fixtures"
LINT_DIR = FIXTURES / "ir_lint"
RUNS_DIR = FIXTURES / "runs"
# run を記録する課題と、記録する壊し方（HEAD の検証で保存できるもの）。
RUN_TASKS = (
    "basic-summary-file",
    "basic-branch-amount",
    "basic-map-rows",
    "app02-triage",
    "app03-weekly-summary",
    "app06-stock-move",
    "app11-overdue",
)
RUN_MUTATIONS = ("escape_var", "input_key")


def ir_sha(ir: dict) -> str:
    return sha256_hex(canonical_json(ir))


def record_lint() -> None:
    bin_path = find_lint_bin()
    if bin_path is None:
        raise SystemExit(
            "ir_lint がありません（cargo build -p shiki-workflow-engine --example ir_lint）"
        )
    LINT_DIR.mkdir(parents=True, exist_ok=True)
    cat = catalog_from_bin(bin_path)
    (LINT_DIR / "catalog.json").write_text(
        json.dumps(cat, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    items = lint_corpus(load_tasks())
    results, meta = run_ir_lint(items, bin_path)
    shas = {i: ir_sha(ir) for i, ir in items}
    with open(LINT_DIR / "core.jsonl", "w", encoding="utf-8") as fh:
        for i, _ in items:
            r = results[i]
            line = {
                "id": i,
                "ir_sha": shas[i],
                "errors": list(r.errors),
                "ignored_by_env": list(r.ignored_by_env),
                "warnings": list(r.warnings),
                "run_blockers": list(r.run_blockers),
                "node_ports": {k: list(v) for k, v in r.node_ports.items()},
                "declared_scopes": list(r.declared_scopes),
            }
            if r.input_error:
                line["input_error"] = r.input_error
            fh.write(json.dumps(line, ensure_ascii=False, sort_keys=True) + "\n")
        fh.write(json.dumps({"ir_lint": meta}) + "\n")
    print(f"ir_lint: {len(items)} 件 → {LINT_DIR / 'core.jsonl'}")


def _write_run(task_id: str, variant: str, case_id: str, saved: dict[str, Saved], rec) -> None:
    RUNS_DIR.mkdir(parents=True, exist_ok=True)
    doc = {
        "task": task_id,
        "variant": variant,
        "case": case_id,
        "irs": {r: s.ir for r, s in saved.items()},
        "node_ports": {r: s.node_ports for r, s in saved.items()},
        "record": rec.to_dict(),
    }
    path = RUNS_DIR / f"{task_id}.{variant}.{case_id}.json"
    path.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"記録: {path.name}（{[r.status for r in rec.runs]}）")


def _save_ir(client: ShikiClient, task, rec, role: str, ir: dict, name: str) -> Saved:
    body = prepare_ir(ir, rec, task, name=name, stub=True)
    v = client.validate(body)
    if v.get("errors"):
        raise SystemExit(f"{name}: 検証を通りません: {v['errors']}")
    created = client.create_workflow(body)
    rec.add("workflow", created["id"], name)
    return Saved(created["id"], 1, v.get("node_ports") or {}, body, v)


def record_runs(api_url: str) -> None:
    tasks = {t.id: t for t in load_tasks()}
    client = ShikiClient(api_url, "alice", stub_only=True, timeout=60.0)
    client.login()
    sentinel, made = fixtures.init_sentinel(client)
    try:
        for tid in RUN_TASKS:
            task = tasks[tid]
            now = fixtures.now_utc()
            rec = fixtures.create_fixtures(client, task.fixtures, now=now)
            try:
                tag = f"rec-{uuid.uuid4().hex[:6]}"
                variants: dict[str, dict[str, Saved]] = {
                    "oracle": save_workflows(
                        client, task, task.reference_irs(), rec, tag=tag, stub=True
                    )
                }
                null = mutate.null_ir(f"abeval-{tag}-null")
                created = client.create_workflow(null)
                rec.add("workflow", created["id"], null["name"])
                variants["null"] = {"main": Saved(created["id"], 1, {"compute": ["out"]}, null, {})}
                for name in RUN_MUTATIONS:
                    m = mutate.by_name(name)
                    bad = m.apply(task.reference_irs()["main"], task.role(None).expect)
                    if bad is not None:
                        variants[name] = {
                            "main": _save_ir(
                                client,
                                task,
                                rec,
                                "main",
                                bad,
                                f"abeval-{tag}-{name.replace('_', '-')}",
                            )
                        }
                for variant, saved in variants.items():
                    for case in stub_cases(task):
                        crec = execute_case(client, task, case, saved, rec, now=now)
                        _write_run(tid, variant, case.id, saved, crec)
            finally:
                report = fixtures.cleanup(client, rec)
                if not report.ok:
                    print(f"片付けの誤り: {report.errors}", file=sys.stderr)
    finally:
        if made:
            client.delete_folder(sentinel["id"], missing_ok=True)
        client.close()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--lint", action="store_true", help="ir_lint の出力を記録する")
    ap.add_argument("--runs", action="store_true", help="stub のスタックで run を記録する")
    ap.add_argument("--api-url", default="http://localhost:8080")
    args = ap.parse_args(argv)
    if not (args.lint or args.runs):
        ap.error("--lint か --runs を付けます")
    if args.lint:
        record_lint()
    if args.runs:
        record_runs(args.api_url)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
