"""振る舞いの指紋（abeval.behavior・GAP-30 §4.1 の 5・BUG-246）。

- 記録と照合: 偽の検証（IR → 誤りのコード）で、serve が残す指紋と run の前の照合を見る。
- HEAD の検証で組の結果が版の差を表すこと: ir_lint（`-m lint_bin`）と stub のスタック（`-m server`）で見る。
  V0（81a9899b）の結果は、abeval.behavior の docstring の表のとおりコードを読んで決めた（ここでは流さない）。
"""

from __future__ import annotations

import os

import pytest

from abeval import behavior, versions
from abeval.grade.static import find_lint_bin, run_ir_lint
from abeval.shiki import ShikiClient

V0_LIKE = {"port_next": [], "template_var": [], "unknown_folder": []}
HEAD_LIKE = {
    "port_next": ["ir.unknown_port"],
    "template_var": ["ir.bad_template"],
    "unknown_folder": ["ir.unknown_folder"],
}


class FakeValidator:
    """名前（IR の name）ごとに決めた誤りのコードを返す検証の口。"""

    def __init__(self, codes: dict[str, list[str]], warnings: dict[str, list[str]] | None = None):
        self.codes = codes
        self.warnings = warnings or {}
        self.sent: list[dict] = []

    def _key(self, ir: dict) -> str:
        return next(k for k, v in behavior.PROBES.items() if v["name"] == ir["name"])

    def validate(self, ir):
        self.sent.append(ir)
        k = self._key(ir)
        return {
            "errors": [{"code": c, "message": "m"} for c in reversed(self.codes.get(k, []))],
            "warnings": [{"code": c} for c in self.warnings.get(k, [])],
            "declared_scopes": [],
        }


def write_v0() -> None:
    versions.write_record(
        versions.parse_record(
            {
                "label": "v0",
                "commit": "c" * 40,
                "tree_clean": True,
                "binary_sha256": "b" * 64,
                "built_at": "2026-11-06T00:00:00Z",
                "rustc": "rustc 1.96.0",
            }
        )
    )


def test_fingerprint_is_the_sorted_codes_of_each_probe():
    v = FakeValidator(
        {"port_next": ["ir.unknown_port", "ir.a"]}, {"template_var": ["ir.template_escaped_var"]}
    )
    got = behavior.fingerprint(v)
    assert set(got) == set(behavior.PROBES)
    assert got["port_next"] == {"errors": ["ir.a", "ir.unknown_port"], "warnings": []}
    assert got["template_var"]["warnings"] == ["ir.template_escaped_var"]
    assert len(v.sent) == len(behavior.PROBES)


def test_serve_records_and_run_verifies_the_same_fingerprint():
    write_v0()
    entry = behavior.record("v0", FakeValidator(V0_LIKE))
    rec = versions.read_record("v0")
    assert rec.extra["behavior_check"] == entry
    assert entry["probes_sha"] == behavior.PROBES_SHA
    assert entry["results"]["port_next"]["errors"] == []
    # ビルドの項目は書き換えない。
    assert rec.binary_sha256 == "b" * 64
    assert behavior.verify("v0", FakeValidator(V0_LIKE)) is None


def test_a_server_that_behaves_like_another_version_is_refused():
    write_v0()
    behavior.record("v0", FakeValidator(V0_LIKE))
    why = behavior.verify("v0", FakeValidator(HEAD_LIKE))
    assert why and "port_next" in why and "ir.unknown_port" in why and "unknown_folder" in why
    # 1 つだけ違っても止める。
    one = dict(V0_LIKE, template_var=["ir.bad_template"])
    why = behavior.verify("v0", FakeValidator(one))
    assert why and "template_var" in why and "port_next" not in why


def test_no_fingerprint_or_a_fingerprint_of_another_probe_set_is_refused():
    write_v0()
    why = behavior.verify("v0", FakeValidator(V0_LIKE))
    assert why and "指紋がありません" in why and "--record-behavior" in why
    behavior.record("v0", FakeValidator(V0_LIKE))
    rec = versions.read_record("v0")
    old = dict(rec.extra["behavior_check"], probes_sha="0" * 64)
    versions.update_record("v0", {"behavior_check": old})
    why = behavior.verify("v0", FakeValidator(V0_LIKE))
    assert why and "別の組" in why
    assert "版の記録がありません" in behavior.verify("v9", FakeValidator(V0_LIKE))


def test_probes_use_only_what_v0_can_read():
    """V0 の保存時検証が読める形だけ（V0 の Stage A のノード・deny_unknown_fields の項目）を使う。"""
    v0_stage_a = {
        "llm.invoke",
        "storage.list",
    }  # 81a9899b の vocab/node_type.rs の available_stage_a
    for ir in behavior.PROBES.values():
        assert set(ir) <= {"ir_version", "name", "triggers", "nodes", "edges"}
        assert ir["triggers"] == [{"kind": "interactive"}]
        for n in ir["nodes"]:
            assert n["type"] in v0_stage_a and set(n) == {"id", "type", "params"}
            if n["type"] == "llm.invoke":
                # V0 はカタログにモデルがあれば model を要する（ir.missing_model）。
                assert n["params"]["model"] == "default"
        for e in ir["edges"]:
            assert set(e) <= {"from", "from_port", "to"}


LINT = find_lint_bin()


@pytest.mark.lint_bin
@pytest.mark.skipif(LINT is None, reason="ir_lint が無いので飛ばします")
def test_head_validator_rejects_the_probes_that_v0_accepts():
    """HEAD の保存時検証（ir_lint）では、V0 で通る組のうち環境に依らない 2 つが落ちる。"""
    got, _ = run_ir_lint(list(behavior.PROBES.items()), LINT)
    assert "ir.unknown_port" in {e["code"] for e in got["port_next"].errors}
    assert "ir.bad_template" in {e["code"] for e in got["template_var"].errors}


API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")


@pytest.mark.server
@pytest.mark.skipif(not API, reason="ABEVAL_API_URL が無いので飛ばします")
def test_head_server_fingerprint_shows_the_version_differences():
    """stub のスタック（HEAD）の検証で、3 つとも V0 と違う誤りになり、送り直しても同じ。"""
    with ShikiClient(API, "alice", stub_only=True, timeout=60.0) as alice:
        first = behavior.fingerprint(alice)
        second = behavior.fingerprint(alice)
    assert first == second
    for name, codes in HEAD_LIKE.items():
        assert set(codes) <= set(first[name]["errors"]), (name, first[name])
