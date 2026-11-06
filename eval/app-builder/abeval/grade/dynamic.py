"""動的な採点（GAP-30 §5.3）。IR を stub の LLM で（Mode B では実モデルでも）実行した記録から、決まった誤りを見つける。

- **dead edge**（`dead_edge`）: 成功した step のノードから出る線のうち、(a) `from_port` がそのノードの出口
  （`node_ports`・検証 API か ir_lint の出力）に無いもの、(b) 出口が 1 つ（error・timeout を除く）のノードで、その
  step の `taken_ports` に無い出口から出るもの。分岐・switch など出口が 2 つ以上あるノードの、通らなかった出口は
  見ない（通らないのは正しい）。error・timeout の出口も見ない。出口は Python で導かず `node_ports` を使う。
- **差し込みの残り**（`leftover`）: step の出力・増えたか変わった行の値・増えたファイルの名前と中身に、`{name}`
  （英数字の名前を 1 重の波括弧で囲んだもの）か `{{…}}` が残っている。
- **入力が届かない**（`not_delivered`）: `flow_through` の入力の値が、LLM の出力（stub は「回答: <解決した prompt>」を
  返す）・データの書き込みの出力・行・ファイルのどれにも現れない（空の差し込み・入力の参照ミス）。
- **llm_dependent** の run_case を stub で流したときは、検査を最初の LLM のノード（llm.invoke・ai.decide・
  agent.invoke）まで（そのノードを含む）に限る。LLM の後ろの動きは stub の出力で決まるので見ない。行とファイルも
  見ない（どのノードが書いたかを決められないため）。
"""

from __future__ import annotations

import re
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import asdict, dataclass, field
from typing import Any

from abeval.grade.checks import CaseResult, normalize, text_of
from abeval.grade.static import LLM_NODE_TYPES, Finding
from abeval.tasks import Expect

NON_ROUTING = frozenset({"error", "timeout"})
_LEFT_SINGLE = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]{0,63})\}(?!\})")
_LEFT_MUSTACHE = re.compile(r"\{\{\s*[^{}\s\"'][^{}\"']{0,80}?\}\}")
_WRITE_TYPES = ("data.record.", "data.transition", "data.bulk_upsert")


@dataclass
class StepRecord:
    step_path: str
    node_id: str
    status: str
    taken_ports: list[str] = field(default_factory=list)
    output: Any = None
    error: Any = None


@dataclass
class RunRecord:
    """1 回の起動の記録（`GET /workflows/{id}/runs/{run_id}` と、成功した step の出力）。"""

    run_id: str | None
    status: str
    input: Mapping[str, Any]
    steps: list[StepRecord]
    workflow: str | None = None
    scheduled_at: str | None = None
    fail_reason: str | None = None

    def outputs(self) -> dict[str, Any]:
        """ノード id → 最後に成功した step の出力。"""
        out: dict[str, Any] = {}
        for s in self.steps:
            if s.status == "succeeded":
                out[s.node_id] = s.output
        return out


@dataclass
class CaseRecord:
    """1 つの run_case の記録（起動の列・行・ファイル）。テストの記録（tests/fixtures/runs/）にもこの形で残す。"""

    runs: list[RunRecord]
    rows: dict[str, list]
    rows_before: dict[str, list]
    files: list[dict]
    rows_rerun: dict[str, list] | None = None
    now: str | None = None

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: Mapping) -> CaseRecord:
        runs = [
            RunRecord(
                run_id=r.get("run_id"),
                status=r["status"],
                input=r.get("input") or {},
                steps=[StepRecord(**s) for s in r.get("steps") or []],
                workflow=r.get("workflow"),
                scheduled_at=r.get("scheduled_at"),
                fail_reason=r.get("fail_reason"),
            )
            for r in d["runs"]
        ]
        return cls(
            runs=runs,
            rows=dict(d.get("rows") or {}),
            rows_before=dict(d.get("rows_before") or {}),
            files=list(d.get("files") or []),
            rows_rerun=d.get("rows_rerun"),
            now=d.get("now"),
        )

    def case_result(self) -> CaseResult:
        """照合子の材料にする。"""
        runs = [{"status": r.status, "input": r.input, "outputs": r.outputs()} for r in self.runs]
        subject = {
            "status": self.runs[-1].status if self.runs else None,
            "runs": runs,
            "outputs": runs[-1]["outputs"] if runs else {},
        }
        return CaseResult(
            statuses=[r.status for r in self.runs],
            subject=subject,
            rows=self.rows,
            rows_before=self.rows_before,
            files=self.files,
            rows_rerun=self.rows_rerun,
        )


# --- LLM の手前に限る ---


def _nodes(ir: Mapping) -> dict[str, Mapping]:
    return {str(n.get("id")): n for n in ir.get("nodes") or [] if isinstance(n, Mapping)}


def _edges(ir: Mapping) -> list[Mapping]:
    return [e for e in ir.get("edges") or [] if isinstance(e, Mapping)]


def downstream(ir: Mapping, start: str) -> set[str]:
    """`start` より後ろで動くノード（線の先・map の中・map の中のノードなら map の線の先も）。`start` は含めない。"""
    nodes = _nodes(ir)
    children: dict[str, list[str]] = {}
    for nid, n in nodes.items():
        if n.get("parent"):
            children.setdefault(str(n["parent"]), []).append(nid)
    out_edges: dict[str, list[str]] = {}
    for e in _edges(ir):
        out_edges.setdefault(str(e.get("from")), []).append(str(e.get("to")))

    def succ(n: str) -> Iterator[str]:
        yield from out_edges.get(n, [])
        yield from children.get(n, [])
        p = nodes.get(n, {}).get("parent")
        while p:
            yield from out_edges.get(str(p), [])
            p = nodes.get(str(p), {}).get("parent")

    seen: set[str] = set()
    stack = list(succ(start))
    while stack:
        n = stack.pop()
        if n in seen:
            continue
        seen.add(n)
        stack.extend(succ(n))
    seen.discard(start)
    return seen


def before_llm(ir: Mapping) -> set[str]:
    """最初の LLM のノードまで（そのノードを含む）。LLM のノードの後ろで動くノードを除いたもの。"""
    nodes = _nodes(ir)
    after: set[str] = set()
    for nid, n in nodes.items():
        if n.get("type") in LLM_NODE_TYPES:
            after |= downstream(ir, nid)
    return set(nodes) - after


# --- 検査 ---


def dead_edges(
    ir: Mapping,
    node_ports: Mapping[str, Sequence[str]],
    run: RunRecord,
    allowed: set[str] | None = None,
) -> list[Finding]:
    taken: dict[str, set[str]] = {}
    for s in run.steps:
        if s.status == "succeeded":
            taken.setdefault(s.node_id, set()).update(s.taken_ports or [])
    out = []
    for e in _edges(ir):
        src = str(e.get("from"))
        port = e.get("from_port") or "out"
        if src not in taken or (allowed is not None and src not in allowed):
            continue
        ports = set(node_ports.get(src) or ())
        routing = ports - NON_ROUTING
        if port not in ports:
            out.append(
                Finding(
                    "dead_edge",
                    src,
                    f"{src} -[{port}]-> {e.get('to')}: {src} の出口は {sorted(ports)}",
                )
            )
        elif port not in NON_ROUTING and len(routing) == 1 and port not in taken[src]:
            out.append(
                Finding(
                    "dead_edge",
                    src,
                    f"{src} -[{port}]-> {e.get('to')}: 通った出口は {sorted(taken[src])}",
                )
            )
    return out


def _strings(value: Any) -> Iterator[str]:
    if isinstance(value, str):
        yield value
    elif isinstance(value, Mapping):
        for k, v in value.items():
            yield str(k)
            yield from _strings(v)
    elif isinstance(value, list | tuple):
        for v in value:
            yield from _strings(v)


def _leftovers_in(text: str) -> list[str]:
    return [m.group(0) for m in _LEFT_SINGLE.finditer(text)] + [
        m.group(0) for m in _LEFT_MUSTACHE.finditer(text)
    ]


def _changed_rows(rec: CaseRecord) -> Iterator[tuple[str, Mapping]]:
    for ref, rows in rec.rows.items():
        before = {r.get("id"): normalize(r.get("data")) for r in rec.rows_before.get(ref) or []}
        for r in rows:
            if before.get(r.get("id"), object()) != normalize(r.get("data")):
                yield ref, r


def leftovers(rec: CaseRecord, allowed: set[str] | None = None) -> list[Finding]:
    found: list[Finding] = []
    seen: set[tuple[str, str]] = set()

    def scan(where: str, node: str | None, value: Any):
        for s in _strings(value):
            for hit in _leftovers_in(s):
                if (where, hit) not in seen:
                    seen.add((where, hit))
                    found.append(Finding("leftover", node, f"{where} に {hit!r} が残る"))

    for run in rec.runs:
        for s in run.steps:
            if s.status == "succeeded" and (allowed is None or s.node_id in allowed):
                scan(f"{s.step_path} の出力", s.node_id, s.output)
    if allowed is None:
        for ref, r in _changed_rows(rec):
            scan(f"{ref} の行", None, r.get("data"))
        for f in rec.files:
            scan(f"ファイル {f.get('name')}", None, [f.get("name"), f.get("text")])
    return found


def _type_of(ir: Mapping, node_id: str) -> str:
    return str(_nodes(ir).get(node_id, {}).get("type", ""))


def _sinks(ir: Mapping, rec: CaseRecord, run: RunRecord, allowed: set[str] | None) -> list[Any]:
    """入力が届いたかを見る先（LLM の出力・データの書き込みの出力・行・ファイル）。"""
    sinks: list[Any] = []
    for s in run.steps:
        if s.status != "succeeded" or (allowed is not None and s.node_id not in allowed):
            continue
        t = _type_of(ir, s.node_id)
        if t in LLM_NODE_TYPES or t.startswith(_WRITE_TYPES):
            sinks.append(s.output)
    if allowed is None:
        sinks += [r.get("data") for _, r in _changed_rows(rec)]
        sinks += [[f.get("name"), f.get("text")] for f in rec.files]
    return sinks


def _scalars(value: Any) -> Iterator[Any]:
    if isinstance(value, Mapping):
        for v in value.values():
            yield from _scalars(v)
    elif isinstance(value, list | tuple):
        for v in value:
            yield from _scalars(v)
    elif value is not None:
        yield value


def _reaches(want: Any, sinks: list[Any]) -> bool:
    leaves = list(_scalars(sinks))
    if isinstance(want, str):
        w = text_of(want).strip()
        return any(isinstance(x, str) and w in text_of(x) for x in leaves) or any(
            normalize(x) == normalize(want) for x in leaves
        )
    num = normalize(want)
    pat = re.compile(rf"(?<![\d.]){re.escape(str(num))}(?![\d])")
    for x in leaves:
        if normalize(x) == num:
            return True
        if isinstance(x, str) and pat.search(text_of(x).replace(",", "")):
            return True
    return False


def not_delivered(
    ir: Mapping, expect: Expect, rec: CaseRecord, run: RunRecord, allowed: set[str] | None = None
) -> list[Finding]:
    sinks = _sinks(ir, rec, run, allowed)
    out = []
    for key in expect.flow_through:
        if key not in run.input:
            continue
        values = run.input[key] if isinstance(run.input[key], list) else [run.input[key]]
        for v in values:
            if isinstance(v, bool) or v is None or isinstance(v, Mapping):
                continue
            if not _reaches(v, sinks):
                out.append(
                    Finding(
                        "not_delivered", None, f"入力 {key}={v!r} が LLM・行・ファイルに届かない"
                    )
                )
    return out


@dataclass
class DynamicGrade:
    findings: list[Finding]
    statuses: list[str]
    restricted: bool

    @property
    def s2d(self) -> bool:
        return bool(self.findings)

    @property
    def s3(self) -> bool:
        return bool(self.statuses) and all(s == "succeeded" for s in self.statuses)

    def detectors(self) -> set[str]:
        return {f"dyn:{f.code}" for f in self.findings}

    def to_dict(self) -> dict:
        return {
            "s2d": self.s2d,
            "s3": self.s3,
            "restricted_to_before_llm": self.restricted,
            "statuses": self.statuses,
            "findings": [f.to_dict() for f in self.findings],
        }


def grade_dynamic(
    irs: Mapping[str, Mapping],
    node_ports: Mapping[str, Mapping[str, Sequence[str]]],
    expects: Mapping[str, Expect],
    rec: CaseRecord,
    *,
    restrict: bool,
    delivery: bool = True,
) -> DynamicGrade:
    """run_case の記録から dead edge・差し込みの残り・入力が届かないを見つける。

    `restrict` は llm_dependent の run_case を stub で流したとき（最初の LLM のノードまでに限る）。
    `delivery=False` は、入力が届かないことが正しい run_case（何も変えないのが正解・`expect.flow_through: false`）。
    役割（`RunRecord.workflow`・無ければ main）ごとに IR・出口・期待を引く。
    """
    findings: list[Finding] = []
    allowed_any: set[str] | None = set() if restrict else None
    for run in rec.runs:
        role = run.workflow or "main"
        ir = irs[role]
        allowed = before_llm(ir) if restrict else None
        if allowed is not None and allowed_any is not None:
            allowed_any |= allowed
        findings += dead_edges(ir, node_ports.get(role) or {}, run, allowed)
        if delivery:
            findings += not_delivered(ir, expects[role], rec, run, allowed)
    findings += leftovers(rec, allowed_any)
    return DynamicGrade(findings, [r.status for r in rec.runs], restrict)
