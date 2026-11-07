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
from abeval.grade.static import (
    LINT_SCHEMA,
    LintCatalog,
    catalog_from_bin,
    find_lint_bin,
    run_ir_lint,
)
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
            "output_paths": list(r.output_paths),
            "node_ports": {k: list(v) for k, v in r.node_ports.items()},
            "declared_scopes": list(r.declared_scopes),
            **({"input_error": r.input_error} if r.input_error else {}),
        }
        assert json.loads(json.dumps(got)) == _plain(want[i]), i
    assert meta["schema"] == LINT_SCHEMA


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


def _uses(paths: list[dict]) -> dict:
    """出力を読むノードの前に、読まれるノードを並べた IR（読み方は `paths` の参照）。"""
    script = {"source": {"inline": "function main(i){ return i; }"}}
    return {
        "ir_version": 1,
        "name": "wf",
        "declared_scopes": [],
        "triggers": [{"kind": "interactive"}],
        "nodes": [
            {"id": "plain", "type": "llm.invoke", "params": {"prompt": "x"}},
            {
                "id": "shaped",
                "type": "llm.invoke",
                "params": {
                    "prompt": "x",
                    "output_schema": {
                        "type": "object",
                        "properties": {"category": {"type": "string"}},
                    },
                },
            },
            {"id": "q", "type": "data.query", "params": {"table": "t", "query": {}}},
            {
                "id": "agg",
                "type": "data.query",
                "params": {"table": "t", "query": {"aggregate": [{"metric": "count"}]}},
            },
            {
                "id": "soft",
                "type": "data.query",
                "on_error": "continue",
                "params": {"table": "t", "query": {}},
            },
            {"id": "s", "type": "script.run", "params": {**script, "input": {}}},
            {"id": "use", "type": "script.run", "params": {**script, "input": {"v": paths}}},
        ],
        "edges": [
            {"from": "plain", "to": "shaped"},
            {"from": "shaped", "to": "q"},
            {"from": "q", "to": "agg"},
            {"from": "agg", "to": "soft"},
            {"from": "soft", "to": "s"},
            {"from": "s", "to": "use"},
        ],
    }


def test_output_paths_follow_the_catalog_fields_and_present_if():
    """ir_lint の出力の path の検査（GAP-112）: カタログの項目・present_if・on_error continue の error。"""
    refs = [
        {"$from": "nodes.plain.output", "path": "/text"},
        {"$from": "nodes.plain.output", "path": "/category"},
        {"$from": "nodes.shaped.output", "path": "/data/category"},
        {"$from": "nodes.plain.output", "path": "/data"},
        {"$from": "nodes.q.output", "path": "/items/0/id"},
        {"$from": "nodes.q.output", "path": "/groups"},
        {"$from": "nodes.agg.output", "path": "/groups/0/value"},
        {"$from": "nodes.soft.output", "path": "/error/message"},
        {"$from": "nodes.q.output", "path": "/error/message"},
        {"$from": "nodes.s.output", "path": "/anything"},
        {"$from": "nodes.q.output"},
    ]
    r = run_ir_lint([("p", _uses(refs))], BIN)[0]["p"]
    bad = sorted(e["path"] for e in r.output_paths)
    assert bad == [
        "/params/input/v/1",
        "/params/input/v/3",
        "/params/input/v/5",
        "/params/input/v/8",
    ], r.output_paths
    assert {e["code"] for e in r.output_paths} == {"ir_lint.output_path"}
    assert {e["node_id"] for e in r.output_paths} == {"use"}


@pytest.mark.parametrize("with_count", [True, False, None])
def test_present_if_treats_false_as_absent(with_count):
    """製品の present_if の約束どおり、false も「値が無い」（with_count: false の /count は読めない）。"""
    ir = _uses([{"$from": "nodes.q.output", "path": "/count"}])
    if with_count is not None:
        ir["nodes"][2]["params"]["query"]["with_count"] = with_count
    r = run_ir_lint([("c", ir)], BIN)[0]["c"]
    assert bool(r.output_paths) is (with_count is not True), r.output_paths
