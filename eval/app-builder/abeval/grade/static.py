"""静的な採点（GAP-30 §5.2）。

**1 層目: ir_lint**（HEAD の `complete_declared_scopes` → `validate` → `save_warnings`・`node_ports`・`run_blockers`・
`enable_blockers`）。標準入力の JSONL を渡し、1 行ずつの結果を読む（`crates/workflow-engine/examples/ir_lint.rs`）。

- `errors`（環境に依存するコード `ignored_by_env` を除いたもの）と `run_blockers`（対話の実行の前に止める理由）は
  S2s（保存できても静的に誤り）に入れる。
- `enable_blockers`（自動実行をオンにする前に止める理由・保存先や一覧のフォルダの省略など）は、課題のきっかけが
  schedule か event のときだけ S2s に入れる（その課題は自動実行をオンにしないと使えない。対話の課題で余計な
  きっかけを足しただけなら数えない）。
- `warnings` のうち [`STRICT_WARNINGS`] は S2s、それ以外は S2w（警告だけ・診断用）に入れる。

  STRICT_WARNINGS の選び方: 実行で結果を壊すか、依頼を満たさないことがほぼ確かなものだけを選ぶ。
  - `ir.template_escaped_var`: `{{x}}` は差し込みにならず「{x}」という文字が出る（BUG-1）。
  - `ir.template_unclosed_brace`: 閉じていない `{` は差し込みにならず、そのまま出る。
  - `ir.unknown_input`: input_schema に無い入力を読む（画面の欄に出ず、課題の入力では値が届かない・入力の参照ミス）。
  - `ir.unconnected_node`: 線が 1 本も無いノード（実行の始めにほかの入口と同時に動くので、前のノードの結果を待たない）。
    製品は意図して並べた入口もあり得るので警告にとどめるが、AI が作った IR ではつなぎ忘れなので S2s に入れる。
  ほかの警告（`ir.condition_semantics_changed`・`ir.effort_ignored`・`ir.required_text_empty` など）は、意図した
  書き方でも出得るか、実行の前に `run_blockers` で別に止まるので S2w に入れる。選び方を変えたら GRADER_VERSION を
  上げる（S2s の率が変わる）。

**2 層目: 課題に依存する検査**（課題の入力と期待から見る。製品の規則は写さない）:

- `input_ref`: `$from input` の path の先頭の key が課題の入力に無い（`default` を書いた参照は除く）。
- `flow_through`: 下流に届くべき入力が、どの値式からも参照されていない。
- `output_path`: `$from nodes.<id>.output` の path の先頭が、そのノードの出力の項目（カタログの `output_fields` と
  `present_if`）に無い。出力の形の決まらないノード（script.run・control.* など項目の無いもの）は見ない。
  `on_error: continue` のノードの `error` は読める。
- S5（構造・診断用）: トリガの種類・cron・tz と、ノードの種類の all_of・any_of・none_of。
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any

from abeval.config import ROOT
from abeval.grade import GRADER_VERSION
from abeval.tasks import Expect

REPO = ROOT.parent.parent
DEFAULT_LINT_BIN = REPO / "target" / "debug" / "examples" / "ir_lint"
STRICT_WARNINGS = frozenset(
    {
        "ir.template_escaped_var",
        "ir.template_unclosed_brace",
        "ir.unknown_input",
        "ir.unconnected_node",
    }
)
LLM_NODE_TYPES = frozenset({"llm.invoke", "ai.decide", "agent.invoke"})


class LintError(RuntimeError):
    """ir_lint を動かせない・出力が読めない。"""


def find_lint_bin() -> Path | None:
    """ir_lint のバイナリ（`ABEVAL_IR_LINT` か `target/debug/examples/ir_lint`）。無ければ None。"""
    env = os.environ.get("ABEVAL_IR_LINT")
    p = Path(env).expanduser() if env else DEFAULT_LINT_BIN
    return p if p.is_file() and os.access(p, os.X_OK) else None


def _require_bin(bin_path: Path | None) -> Path:
    p = bin_path or find_lint_bin()
    if p is None:
        raise LintError(
            "ir_lint がありません（cargo build -p shiki-workflow-engine --example ir_lint か ABEVAL_IR_LINT）"
        )
    return p


def file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def grader_version(bin_path: Path | None = None) -> dict:
    """結果に残す採点器の版（ir_lint の crate_version は 0.0.0 なのでバイナリの sha で区別する）。"""
    p = bin_path or find_lint_bin()
    return {"grader": GRADER_VERSION, "ir_lint_sha256": file_sha256(p) if p else None}


# --- 1 層目: ir_lint ---


@dataclass(frozen=True)
class LintResult:
    id: str | None
    errors: tuple[dict, ...] = ()
    ignored_by_env: tuple[dict, ...] = ()
    warnings: tuple[dict, ...] = ()
    run_blockers: tuple[dict, ...] = ()
    enable_blockers: tuple[dict, ...] = ()
    node_ports: Mapping[str, tuple[str, ...]] = field(default_factory=dict)
    declared_scopes: tuple[str, ...] = ()
    input_error: str | None = None

    @classmethod
    def from_json(cls, d: Mapping) -> LintResult:
        return cls(
            id=d.get("id"),
            errors=tuple(d.get("errors") or ()),
            ignored_by_env=tuple(d.get("ignored_by_env") or ()),
            warnings=tuple(d.get("warnings") or ()),
            run_blockers=tuple(d.get("run_blockers") or ()),
            enable_blockers=tuple(d.get("enable_blockers") or ()),
            node_ports={k: tuple(v) for k, v in (d.get("node_ports") or {}).items()},
            declared_scopes=tuple(d.get("declared_scopes") or ()),
            input_error=d.get("input_error"),
        )


def run_ir_lint(
    items: Iterable[tuple[str, Mapping]], bin_path: Path | None = None, timeout: float = 120.0
) -> tuple[dict[str, LintResult], dict]:
    """IR を ir_lint に掛ける。(id → 結果, 版の行)。id は重ねない。"""
    p = _require_bin(bin_path)
    items = list(items)
    ids = [i for i, _ in items]
    if len(set(ids)) != len(ids):
        raise ValueError("ir_lint に渡す id が重複しています")
    stdin = "".join(json.dumps({"id": i, "ir": ir}, ensure_ascii=False) + "\n" for i, ir in items)
    done = subprocess.run(
        [str(p)], input=stdin, capture_output=True, text=True, timeout=timeout, check=False
    )
    if done.returncode != 0:
        raise LintError(
            f"ir_lint が終了コード {done.returncode} で止まりました: {done.stderr[:500]}"
        )
    return parse_lint_output(done.stdout, ids)


def parse_lint_output(
    text: str, ids: list[str] | None = None
) -> tuple[dict[str, LintResult], dict]:
    """ir_lint の出力（JSONL）を読む。最後の行は版の行。"""
    out: dict[str, LintResult] = {}
    meta: dict | None = None
    for line in text.splitlines():
        if not line.strip():
            continue
        d = json.loads(line)
        if "ir_lint" in d and "id" not in d:
            meta = d["ir_lint"]
            continue
        r = LintResult.from_json(d)
        if r.id is None:
            raise LintError(f"ir_lint が id の無い行を返しました: {line[:200]}")
        out[r.id] = r
    if meta is None:
        raise LintError("ir_lint の出力に版の行がありません")
    if ids is not None and sorted(out) != sorted(ids):
        raise LintError(f"ir_lint の結果の id が入力と違います: {sorted(set(ids) ^ set(out))}")
    return out, meta


@dataclass(frozen=True)
class NodeInfo:
    type: str
    available: bool
    output_ports: tuple[str, ...]
    port_rule: str
    output_fields: tuple[tuple[str, str], ...]  # (名前, present_if)


@dataclass(frozen=True)
class LintCatalog:
    """`ir_lint --catalog` の中身（ノードのカタログ・警告のコード・環境に依存するコード）。"""

    nodes: Mapping[str, NodeInfo]
    save_warning_codes: tuple[str, ...]
    ignored_by_env_codes: tuple[str, ...]
    meta: Mapping[str, Any]

    @classmethod
    def from_json(cls, d: Mapping) -> LintCatalog:
        nodes = {}
        for e in d["node_catalog"]:
            nodes[e["type"]] = NodeInfo(
                type=e["type"],
                available=bool(e.get("available")),
                output_ports=tuple(e.get("output_ports") or ()),
                port_rule=e.get("port_rule") or "static",
                output_fields=tuple(
                    (f["name"], f.get("present_if") or "") for f in e.get("output_fields") or ()
                ),
            )
        return cls(
            nodes=nodes,
            save_warning_codes=tuple(d["save_warning_codes"]),
            ignored_by_env_codes=tuple(d["ignored_by_env_codes"]),
            meta=dict(d.get("ir_lint") or {}),
        )

    def port_names(self) -> frozenset[str]:
        """カタログに載る決まった出口の名前と、どのノードにも付き得る出口（error・timeout・default）。"""
        names = {p for n in self.nodes.values() for p in n.output_ports}
        return frozenset(names | {"error", "timeout", "default"})


def catalog_from_bin(bin_path: Path | None = None) -> dict:
    p = _require_bin(bin_path)
    done = subprocess.run(
        [str(p), "--catalog"], capture_output=True, text=True, timeout=60, check=False
    )
    if done.returncode != 0:
        raise LintError(f"ir_lint --catalog が失敗しました: {done.stderr[:500]}")
    return json.loads(done.stdout.splitlines()[0])


@lru_cache(maxsize=4)
def load_catalog(path: str | None = None) -> LintCatalog:
    """カタログ（`path` は記録した JSON・無ければバイナリから）。"""
    if path:
        return LintCatalog.from_json(json.loads(Path(path).read_text(encoding="utf-8")))
    return LintCatalog.from_json(catalog_from_bin())


@dataclass(frozen=True)
class Layer1:
    errors: tuple[str, ...]
    run_blockers: tuple[str, ...]
    strict_warnings: tuple[str, ...]
    warnings: tuple[str, ...]
    ignored_by_env: tuple[str, ...]
    input_error: str | None
    # 自動実行をオンにする前に止める理由（きっかけが schedule・event の課題だけ・それ以外は空）。
    enable_blockers: tuple[str, ...] = ()

    @property
    def s2s(self) -> bool:
        return bool(
            self.errors
            or self.run_blockers
            or self.enable_blockers
            or self.strict_warnings
            or self.input_error
        )


UNATTENDED = ("schedule", "event")


def classify_lint(r: LintResult, unattended: bool = False) -> Layer1:
    """ir_lint の 1 行を S2s と S2w に分ける（`unattended` は課題のきっかけが schedule・event）。"""
    warn = [w["code"] for w in r.warnings]
    return Layer1(
        errors=tuple(e["code"] for e in r.errors),
        run_blockers=tuple(e["code"] for e in r.run_blockers),
        strict_warnings=tuple(c for c in warn if c in STRICT_WARNINGS),
        warnings=tuple(c for c in warn if c not in STRICT_WARNINGS),
        ignored_by_env=tuple(e["code"] for e in r.ignored_by_env),
        input_error=r.input_error,
        enable_blockers=tuple(e["code"] for e in r.enable_blockers) if unattended else (),
    )


# --- 2 層目: 課題に依存する検査 ---


@dataclass(frozen=True)
class Finding:
    code: str
    node_id: str | None
    detail: str

    def to_dict(self) -> dict:
        return {"code": self.code, "node_id": self.node_id, "detail": self.detail}


def pointer_tokens(path: str | None) -> list[str]:
    """JSON Pointer の各段（~1 → /・~0 → ~）。None・空は根。"""
    if not path:
        return []
    if not path.startswith("/"):
        return [path]
    return [t.replace("~1", "/").replace("~0", "~") for t in path[1:].split("/")]


def iter_refs(value: Any, at: str = "") -> Iterator[tuple[str, Mapping]]:
    """値の中の参照 `{"$from": …}`（(場所, 参照)）。"""
    if isinstance(value, Mapping):
        if isinstance(value.get("$from"), str):
            yield at, value
        for k, v in value.items():
            yield from iter_refs(v, f"{at}/{k}")
    elif isinstance(value, list):
        for i, v in enumerate(value):
            yield from iter_refs(v, f"{at}/{i}")


def iter_node_refs(ir: Mapping) -> Iterator[tuple[str, str, Mapping]]:
    """ノードの params の中の参照（(ノード id, 場所, 参照)）。"""
    for n in ir.get("nodes") or []:
        if isinstance(n, Mapping):
            for at, ref in iter_refs(n.get("params"), "/params"):
                yield str(n.get("id")), at, ref


def _resolve(value: Any, tokens: list[str]) -> Any:
    for t in tokens:
        if isinstance(value, Mapping) and t in value:
            value = value[t]
        elif isinstance(value, list) and t.isdigit() and int(t) < len(value):
            value = value[int(t)]
        else:
            return None
    return value


def _present(params: Any, present_if: str) -> bool:
    """設定のその場所に値があるか（製品の `OutputField::present_if` と同じく、無い・false・空の文字・空のリスト・
    空のオブジェクトは「値が無い」）。"""
    if not present_if:
        return True
    v = _resolve(params, pointer_tokens(present_if))
    return not (v is None or v is False or v == "" or v == [] or v == {})


def input_ref_findings(ir: Mapping, expect: Expect) -> list[Finding]:
    keys = expect.input_keys
    out = []
    for node, at, ref in iter_node_refs(ir):
        if ref["$from"] != "input" or "default" in ref:
            continue
        tokens = pointer_tokens(ref.get("path"))
        if tokens and tokens[0] not in keys:
            out.append(
                Finding(
                    "input_ref",
                    node,
                    f"{at}: 入力に無い key を読む: /{tokens[0]}（入力は {sorted(keys)}）",
                )
            )
    return out


def flow_through_findings(ir: Mapping, expect: Expect) -> list[Finding]:
    read: set[str] = set()
    whole = False
    for _, _, ref in iter_node_refs(ir):
        if ref["$from"] != "input":
            continue
        tokens = pointer_tokens(ref.get("path"))
        if tokens:
            read.add(tokens[0])
        else:
            whole = True
    if whole:
        return []
    return [
        Finding("flow_through", None, f"入力 {k!r} をどの値式も読まない（下流に届かない）")
        for k in expect.flow_through
        if k not in read
    ]


def output_path_findings(ir: Mapping, catalog: LintCatalog) -> list[Finding]:
    nodes = {str(n.get("id")): n for n in ir.get("nodes") or [] if isinstance(n, Mapping)}
    out = []
    for node, at, ref in iter_node_refs(ir):
        src = ref["$from"]
        if not (src.startswith("nodes.") and src.endswith(".output")):
            continue
        target = nodes.get(src[len("nodes.") : -len(".output")])
        tokens = pointer_tokens(ref.get("path"))
        if target is None or not tokens:
            continue  # 無いノードは ir_lint（bad_ref）が見る
        info = catalog.nodes.get(str(target.get("type")))
        if info is None or not info.output_fields:
            continue  # 出力の形が決まらないノード
        allowed = {
            name
            for name, present_if in info.output_fields
            if _present(target.get("params"), present_if)
        }
        if target.get("on_error") == "continue":
            allowed.add("error")
        if tokens[0] not in allowed:
            out.append(
                Finding(
                    "output_path",
                    node,
                    f"{at}: {target.get('id')}（{info.type}）の出力に /{tokens[0]} はありません（{sorted(allowed)}）",
                )
            )
    return out


def _norm_cron(c: str) -> str:
    return " ".join(str(c).split())


def structure_findings(ir: Mapping, expect: Expect) -> list[Finding]:
    """S5（診断用）: トリガとノードの種類。"""
    out = []
    trig = expect.trigger
    triggers = [t for t in ir.get("triggers") or [] if isinstance(t, Mapping)]
    if trig:
        same = [t for t in triggers if t.get("kind") == trig["kind"]]
        if not same:
            got = [t.get("kind") for t in triggers]
            out.append(
                Finding("trigger_kind", None, f"きっかけ {trig['kind']} がありません（{got}）")
            )
        elif trig["kind"] == "schedule":
            crons = {_norm_cron(c) for c in trig["cron_any"]}
            if not any(_norm_cron(t.get("cron", "")) in crons for t in same):
                out.append(
                    Finding(
                        "trigger_cron",
                        None,
                        f"cron が {sorted(crons)} ではありません: {[t.get('cron') for t in same]}",
                    )
                )
            if not any(t.get("tz") == trig["tz"] for t in same):
                out.append(Finding("trigger_tz", None, f"tz が {trig['tz']} ではありません"))
    types = {str(n.get("type")) for n in ir.get("nodes") or [] if isinstance(n, Mapping)}
    nt = expect.node_types
    for t in nt.get("all_of") or []:
        if t not in types:
            out.append(Finding("node_all_of", None, f"{t} がありません"))
    for group in nt.get("any_of") or []:
        if not types & set(group):
            out.append(Finding("node_any_of", None, f"{group} のどれもありません"))
    for t in nt.get("none_of") or []:
        if t in types:
            out.append(Finding("node_none_of", None, f"使わないはずの {t} があります"))
    return out


@dataclass
class StaticGrade:
    layer1: Layer1
    task_findings: list[Finding]
    structure: list[Finding]

    @property
    def s2s(self) -> bool:
        return self.layer1.s2s or bool(self.task_findings)

    @property
    def s2w(self) -> bool:
        return not self.s2s and bool(self.layer1.warnings)

    @property
    def s5(self) -> bool:
        return not self.structure

    def detectors(self) -> set[str]:
        """捕まえた検査の名前（採点器の自己テストで、期待した分類で捕まったかを見る）。"""
        l1 = self.layer1
        out = {
            f"lint:{c}"
            for c in (*l1.errors, *l1.strict_warnings, *l1.run_blockers, *l1.enable_blockers)
        }
        out |= {f"task:{f.code}" for f in self.task_findings}
        if self.structure:
            out.add("s5")
        return out

    def to_dict(self) -> dict:
        l1 = self.layer1
        return {
            "s2s": self.s2s,
            "s2w": self.s2w,
            "s5": self.s5,
            "lint": {
                "errors": list(l1.errors),
                "run_blockers": list(l1.run_blockers),
                "enable_blockers": list(l1.enable_blockers),
                "strict_warnings": list(l1.strict_warnings),
                "warnings": list(l1.warnings),
                "ignored_by_env": list(l1.ignored_by_env),
                "input_error": l1.input_error,
            },
            "task": [f.to_dict() for f in self.task_findings],
            "structure": [f.to_dict() for f in self.structure],
        }


def grade_static(
    ir: Mapping, expect: Expect, lint: LintResult, catalog: LintCatalog
) -> StaticGrade:
    """1 つの IR の静的な採点（ir_lint の結果と、課題の期待から）。"""
    task = []
    if isinstance(ir, Mapping):
        task = [
            *input_ref_findings(ir, expect),
            *flow_through_findings(ir, expect),
            *output_path_findings(ir, catalog),
        ]
    structure = structure_findings(ir, expect) if isinstance(ir, Mapping) else []
    unattended = (expect.trigger or {}).get("kind") in UNATTENDED
    return StaticGrade(classify_lint(lint, unattended), task, structure)
