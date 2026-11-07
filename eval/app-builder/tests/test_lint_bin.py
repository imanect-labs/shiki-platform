"""記録した ir_lint の出力（tests/fixtures/ir_lint/）が、いまのバイナリの出力と同じこと（`pytest -m lint_bin`）。

バイナリ（`target/debug/examples/ir_lint` か ABEVAL_IR_LINT）が無ければ理由を出して飛ばす。違っていたら
`uv run python scripts/record_fixtures.py --lint` で記録し直し、採点の結果が変わらないかを見る（製品の検証が
変わったなら、採点器の版を上げる）。
"""

from __future__ import annotations

import json

import pytest

import recorded
from abeval import gen_checks as g
from abeval.grade.static import LintCatalog, catalog_from_bin, find_lint_bin, run_ir_lint
from abeval.tasks import load_tasks

BIN = find_lint_bin()

pytestmark = [
    pytest.mark.lint_bin,
    pytest.mark.skipif(
        BIN is None,
        reason="ir_lint が無いので飛ばします（cargo build -p shiki-workflow-engine --example ir_lint）",
    ),
]


def _plain(d: dict) -> dict:
    return {k: v for k, v in d.items() if k != "ir_sha"}


def test_recorded_lint_matches_the_binary():
    items = g.lint_corpus(load_tasks())
    recorded.check_fresh(items)
    fresh, meta = run_ir_lint(items, BIN)
    want = recorded.lint_lines()
    assert set(fresh) == set(want)
    for i, _ in items:
        r = fresh[i]
        got = {
            "id": i,
            "errors": list(r.errors),
            "ignored_by_env": list(r.ignored_by_env),
            "warnings": list(r.warnings),
            "run_blockers": list(r.run_blockers),
            "enable_blockers": list(r.enable_blockers),
            "node_ports": {k: list(v) for k, v in r.node_ports.items()},
            "declared_scopes": list(r.declared_scopes),
            **({"input_error": r.input_error} if r.input_error else {}),
        }
        assert json.loads(json.dumps(got)) == _plain(want[i]), i
    assert meta["schema"] == 2


def test_recorded_catalog_matches_the_binary():
    live = catalog_from_bin(BIN)
    assert live == json.loads((recorded.LINT_DIR / "catalog.json").read_text(encoding="utf-8"))
    LintCatalog.from_json(live)


def test_g2_with_the_live_binary():
    res = g.g2_reference_passes(load_tasks(), recorded.catalog(), None, BIN)
    assert all(r.ok for r in res), [r.to_dict() for r in res if not r.ok]


def test_unknown_version_is_a_static_error():
    """ir_version の誤りは環境に依存しないので errors に出る（S2s・D-164 (f)）。"""
    ir = {
        "ir_version": 2,
        "name": "wf",
        "declared_scopes": [],
        "nodes": [
            {
                "id": "s",
                "type": "script.run",
                "params": {"source": {"inline": "function main(i){ return i; }"}, "input": {}},
            }
        ],
        "edges": [],
    }
    r = run_ir_lint([("v", ir)], BIN)[0]["v"]
    assert "ir.unknown_version" in {e["code"] for e in r.errors}
    assert "ir.unknown_version" not in {e["code"] for e in r.ignored_by_env}
    assert "ir.unknown_version" not in recorded.catalog().ignored_by_env_codes
