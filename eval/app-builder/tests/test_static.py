"""静的な採点（S2s と S2w の分け方・課題の検査・S5）。ir_lint の出力は記録（tests/fixtures/ir_lint/）を使う。"""

from __future__ import annotations

import json
import os
import stat

import pytest

import recorded
from abeval.grade import static
from abeval.grade.static import (
    LINT_SCHEMA,
    STRICT_WARNINGS,
    LintCatalog,
    LintError,
    LintResult,
    classify_lint,
    flow_through_findings,
    grade_static,
    input_ref_findings,
    parse_lint_output,
    run_ir_lint,
    structure_findings,
)
from abeval.tasks import Expect, InputSpec, offline_irs


def _expect(inputs=(), flow=(), trigger=None, node_types=None) -> Expect:
    return Expect(trigger, node_types or {}, tuple(InputSpec(k, (k,)) for k in inputs), tuple(flow))


def _err(code: str) -> dict:
    return {"code": code, "message": code}


def test_strict_warnings_are_real_save_warning_codes():
    # 手書きの組がカタログ（製品の SAVE_WARNING_CODES）とずれないように。
    codes = set(recorded.catalog().save_warning_codes)
    assert codes, "記録したカタログに警告のコードがありません"
    assert codes >= STRICT_WARNINGS, STRICT_WARNINGS - codes
    assert len(codes - STRICT_WARNINGS) >= 10, "ほかの警告は S2w に入る"


def test_ignored_by_env_comes_from_the_catalog():
    # ir.unknown_version は環境に依存しない静的な誤りなので errors に出る（D-164 (f)）。
    assert set(recorded.catalog().ignored_by_env_codes) == {
        "ir.unknown_secret",
        "ir.binding_denied",
        "ir.unknown_skill",
    }


def test_errors_blockers_and_strict_warnings_are_s2s_other_warnings_are_s2w():
    r = LintResult(
        id="x", warnings=(_err("ir.effort_ignored"), _err("ir.condition_semantics_changed"))
    )
    l1 = classify_lint(r)
    assert not l1.s2s and l1.warnings == ("ir.effort_ignored", "ir.condition_semantics_changed")
    g = static.StaticGrade(l1, [], [])
    assert g.s2w and not g.s2s

    for strict in sorted(STRICT_WARNINGS):
        l1 = classify_lint(LintResult(id="x", warnings=(_err(strict), _err("ir.effort_ignored"))))
        assert l1.s2s and l1.strict_warnings == (strict,) and l1.warnings == ("ir.effort_ignored",)
        assert not static.StaticGrade(l1, [], []).s2w, "S2s があれば S2w にしない"

    assert classify_lint(LintResult(id="x", errors=(_err("ir.bad_ref"),))).s2s
    assert classify_lint(LintResult(id="x", run_blockers=(_err("ir.required_text_empty"),))).s2s
    assert classify_lint(LintResult(id="x", input_error="読めない")).s2s
    env = classify_lint(LintResult(id="x", ignored_by_env=(_err("ir.unknown_secret"),)))
    assert not env.s2s and env.ignored_by_env == ("ir.unknown_secret",)


def test_recorded_lint_lines_split_as_expected():
    lines = recorded.lint()
    # 差し込みを {{x}} にした IR は保存できる（警告）が S2s。
    esc = classify_lint(lines["basic-summary-file/main/mut:escape_var"])
    assert not esc.errors and "ir.template_escaped_var" in esc.strict_warnings
    # 入力の key を替えた IR も警告（ir.unknown_input）で S2s。
    key = classify_lint(lines["basic-summary-file/main/mut:input_key"])
    assert not key.errors and "ir.unknown_input" in key.strict_warnings
    # 正解は何も無い。
    assert not classify_lint(lines["basic-summary-file/main"]).s2s


def test_input_ref_findings():
    ir = {
        "nodes": [
            {
                "id": "a",
                "type": "llm.invoke",
                "params": {
                    "prompt": {
                        "$template": "{t}{u}{w}",
                        "vars": {
                            "t": {"$from": "input", "path": "/title"},
                            "u": {"$from": "input", "path": "/subject"},
                            "w": {"$from": "input", "path": "/note", "default": ""},
                        },
                    }
                },
            },
            {"id": "b", "type": "script.run", "params": {"input": {"$from": "input"}}},
        ]
    }
    found = input_ref_findings(ir, _expect(["title"]))
    assert [f.code for f in found] == ["input_ref"]
    assert "/subject" in found[0].detail and found[0].node_id == "a"


def test_flow_through_findings():
    ir = {
        "nodes": [
            {
                "id": "a",
                "type": "llm.invoke",
                "params": {"prompt": {"$from": "input", "path": "/title/0"}},
            }
        ]
    }
    assert flow_through_findings(ir, _expect(["title", "body"], ["title"])) == []
    missing = flow_through_findings(ir, _expect(["title", "body"], ["title", "body"]))
    assert [f.code for f in missing] == ["flow_through"] and "body" in missing[0].detail
    whole = {"nodes": [{"id": "a", "type": "script.run", "params": {"input": {"$from": "input"}}}]}
    assert flow_through_findings(whole, _expect(["body"], ["body"])) == []


def test_output_paths_come_from_ir_lint_and_count_as_s2s():
    """出力の path の誤りは ir_lint の `output_paths` を読むだけ（Python で判定しない・GAP-112）。"""
    hit = {"code": "ir_lint.output_path", "message": "m", "node_id": "use", "path": "/params/x"}
    lr = LintResult(id="x", output_paths=(hit,))
    l1 = classify_lint(lr)
    assert l1.output_paths == ("ir_lint.output_path",) and l1.s2s
    g = grade_static({"nodes": [], "edges": []}, _expect(), lr, recorded.catalog())
    assert g.s2s and "lint:ir_lint.output_path" in g.detectors()
    assert g.to_dict()["lint"]["output_paths"] == ["ir_lint.output_path"]
    assert g.task_findings == []  # 2 層目（課題の検査）には入らない
    clean = classify_lint(LintResult(id="y"))
    assert clean.output_paths == () and not clean.s2s
    # 記録の行からも読める（record_fixtures.py が書く形）。
    assert LintResult.from_json({"id": "z", "output_paths": [hit]}).output_paths == (hit,)


def test_python_no_longer_judges_output_paths():
    assert not hasattr(static, "output_path_findings")
    assert not hasattr(static, "_present")
    assert not hasattr(static.NodeInfo, "output_fields")


def test_structure_findings_trigger_and_node_types():
    exp = _expect(
        trigger={"kind": "schedule", "cron_any": ["0 9 * * 1"], "tz": "Asia/Tokyo"},
        node_types={
            "all_of": ["data.query"],
            "any_of": [["llm.invoke", "agent.invoke"]],
            "none_of": ["agent.invoke"],
        },
    )
    good = {
        "triggers": [{"kind": "schedule", "cron": "0  9 * * 1", "tz": "Asia/Tokyo"}],
        "nodes": [{"id": "a", "type": "data.query"}, {"id": "b", "type": "llm.invoke"}],
    }
    assert structure_findings(good, exp) == []
    bad = {
        "triggers": [{"kind": "schedule", "cron": "0 8 * * 1", "tz": "UTC"}],
        "nodes": [{"id": "b", "type": "agent.invoke"}],
    }
    codes = sorted(f.code for f in structure_findings(bad, exp))
    assert codes == ["node_all_of", "node_none_of", "trigger_cron", "trigger_tz"]
    inter = {
        "triggers": [{"kind": "interactive"}],
        "nodes": [{"id": "a", "type": "data.query"}, {"id": "b", "type": "llm.invoke"}],
    }
    assert [f.code for f in structure_findings(inter, exp)] == ["trigger_kind"]
    none = {"triggers": [], "nodes": [{"id": "a", "type": "data.query"}]}
    assert "node_any_of" in {f.code for f in structure_findings(none, exp)}


def test_every_core_reference_is_clean_with_recorded_lint():
    cat = recorded.catalog()
    lines = recorded.lint()
    items = []
    for t in recorded.tasks().values():
        for role, ir in offline_irs(t).items():
            items.append((f"{t.id}/{role}", ir))
    recorded.check_fresh(items)
    for key, ir in items:
        task_id, role = key.split("/")
        t = recorded.tasks()[task_id]
        g = grade_static(ir, t.role(role).expect, lines[key], cat)
        assert not g.s2s and not g.s2w and g.s5, (key, g.to_dict())
        assert g.detectors() == set(), key


def test_null_ir_is_lint_clean_but_misses_the_task():
    # 中身の無い IR は ir_lint では誤りが無い（S2s にならない）。落ちるのは S4（照合）と S5。
    from abeval.grade.mutate import null_ir

    recorded.check_fresh([("null", null_ir("null-ir"))])
    cat = recorded.catalog()
    t = recorded.tasks()["basic-summary-file"]
    g = grade_static(null_ir("null-ir"), t.role(None).expect, recorded.lint()["null"], cat)
    assert not g.layer1.s2s and not g.s5
    assert g.s2s, "下流に届くべき入力をどこも読まない（flow_through）"


def _lint_out(schema=LINT_SCHEMA, **line) -> str:
    meta = {} if schema is None else {"schema": schema}
    return (
        json.dumps({"id": "a", "errors": [], **line}) + "\n" + json.dumps({"ir_lint": meta}) + "\n"
    )


def test_parse_lint_output_rejects_broken_output():
    ok = _lint_out()
    res, meta = parse_lint_output(ok, ["a"])
    assert meta == {"schema": LINT_SCHEMA} and res["a"].errors == ()
    with pytest.raises(LintError, match="版の行"):
        parse_lint_output(json.dumps({"id": "a"}) + "\n")
    with pytest.raises(LintError, match="id が入力と違います"):
        parse_lint_output(ok, ["a", "b"])
    with pytest.raises(LintError, match="id の無い行"):
        parse_lint_output(
            json.dumps({"id": None, "input_error": "x"})
            + "\n"
            + json.dumps({"ir_lint": {"schema": LINT_SCHEMA}})
        )


@pytest.mark.parametrize("schema", [2, 1, None, "3", 4])
def test_an_old_or_unknown_lint_output_shape_is_refused(schema):
    """古い ir_lint（schema 2 は output_paths が無い）を空の欄として読まず、作り直しを促して止める（S2s を甘くしない）。"""
    with pytest.raises(LintError, match="出力の形の版") as e:
        parse_lint_output(_lint_out(schema), ["a"])
    assert "cargo build -p shiki-workflow-engine --example ir_lint" in str(e.value)


def test_an_old_catalog_is_refused_too():
    d = json.loads((recorded.LINT_DIR / "catalog.json").read_text(encoding="utf-8"))
    assert LintCatalog.from_json(d).meta["schema"] == LINT_SCHEMA
    with pytest.raises(LintError, match="--catalog の出力の形の版"):
        LintCatalog.from_json({**d, "ir_lint": {**d["ir_lint"], "schema": 2}})


def test_the_grader_version_carries_the_lint_schema(tmp_path, monkeypatch):
    monkeypatch.setenv("ABEVAL_IR_LINT", str(tmp_path / "none"))
    assert static.grader_version()["ir_lint_schema"] == LINT_SCHEMA


def _fake_bin(tmp_path, body: str):
    p = tmp_path / "fake_ir_lint"
    p.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    p.chmod(p.stat().st_mode | stat.S_IXUSR)
    return p


def test_run_ir_lint_feeds_jsonl_and_reads_results(tmp_path):
    # 偽のバイナリ: 入力の行数を数えて、その数だけ id を返す（入力が JSONL で届くことを見る）。
    out = tmp_path / "seen.jsonl"
    p = _fake_bin(
        tmp_path,
        f'cat > "{out}"\n'
        'echo \'{"id":"a","errors":[{"code":"ir.bad_ref","message":"m"}],"warnings":[]}\'\n'
        'echo \'{"id":"b","errors":[],"warnings":[]}\'\n'
        "echo '" + json.dumps({"ir_lint": {"schema": LINT_SCHEMA}}) + "'\n",
    )
    res, meta = run_ir_lint([("a", {"nodes": []}), ("b", {"nodes": [1]})], p)
    sent = [json.loads(line) for line in out.read_text().splitlines()]
    assert [s["id"] for s in sent] == ["a", "b"] and sent[1]["ir"] == {"nodes": [1]}
    assert res["a"].errors[0]["code"] == "ir.bad_ref" and meta == {"schema": LINT_SCHEMA}
    with pytest.raises(ValueError, match="重複"):
        run_ir_lint([("a", {}), ("a", {})], p)
    bad = _fake_bin(tmp_path, "exit 3\n")
    with pytest.raises(LintError, match="終了コード 3"):
        run_ir_lint([("a", {})], bad)


def test_find_lint_bin_honours_the_env(tmp_path, monkeypatch):
    p = _fake_bin(tmp_path, "exit 0\n")
    monkeypatch.setenv("ABEVAL_IR_LINT", str(p))
    assert static.find_lint_bin() == p
    os.chmod(p, 0o644)
    assert static.find_lint_bin() is None
    monkeypatch.setenv("ABEVAL_IR_LINT", str(tmp_path / "none"))
    assert static.find_lint_bin() is None
    with pytest.raises(LintError, match="ir_lint がありません"):
        run_ir_lint([("a", {})])
    v = static.grader_version()
    assert v["ir_lint_sha256"] is None and v["grader"] >= 1
