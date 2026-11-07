"""動的な採点（GAP-30 §5.3）。IR を stub の LLM で（Mode B では実モデルでも）実行した記録から、決まった誤りを見つける。

- **dead edge**（`dead_edge`）: 成功した step のノードから出る線のうち、(a) `from_port` がそのノードの出口
  （`node_ports`・検証 API か ir_lint の出力）に無いもの、(b) 出口が 1 つ（error・timeout を除く）のノードで、その
  step の `taken_ports` に無い出口から出るもの。分岐・switch など出口が 2 つ以上あるノードの、通らなかった出口は
  見ない（通らないのは正しい）。error・timeout の出口も見ない。出口は Python で導かず `node_ports` を使う。
- **差し込みの残り**（`leftover`）: step の出力・増えたか変わった行の値・増えたファイルの名前と中身に、`{name}`
  （英数字の名前を 1 重の波括弧で囲んだもの）か `{{…}}` が残っている。
- **入力が届かない**（`not_delivered`）: `flow_through` の入力の値が、LLM の出力（stub は「回答: <解決した prompt>」を
  返す）・データの書き込みの出力・行・ファイルのどれにも現れない（空の差し込み・入力の参照ミス）。stub の run だけで
  見る（実モデルの出力は入力の全文を含まないので、届いたかを出力から決められない。Mode B は同じ試行で stub の
  配管も流すので、そちらで見る）。
- **想定外の状態**（`unexpected_status`）: stub で流した llm_dependent でない run_case で、run の状態が期待
  （既定は succeeded・`any` は見ない）と違う。flow_through の無い課題でも、実行で失敗する IR を「壊れている」に数える。
- **想定外の skipped**（`unexpected_skip`）: 成功した run の中で、分岐（出口が 2 つ以上のノード）でも error・timeout の
  出口でもない線の先が飛ばされた step（分岐でない上流が `out` を取らなかった・`on_error: continue` で失敗した）。
  飛ばされた上流から続けて飛ばされたものは、元の 1 つだけを数える。期待が succeeded の run_case だけで見る。
- **llm_dependent** の run_case を stub で流したときは、検査を最初の LLM のノード（llm.invoke・ai.decide・
  agent.invoke）まで（そのノードを含む）に限る。LLM の後ろの動きは stub の出力で決まるので見ない。行とファイルも
  見ない（どのノードが書いたかを決められないため）。
"""

from __future__ import annotations

import re
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import asdict, dataclass, field
from typing import Any

from abeval.grade.checks import CaseResult, normalize, run_start_dates, text_of
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
    # run を始めた時刻（`started_at`・無ければ `created_at`）。`$run_date` の照合に使う。
    started_at: str | None = None

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
                started_at=r.get("started_at"),
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
            run_dates=run_start_dates([r.started_at for r in self.runs]),
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


def _input_keys(value: Any) -> Iterator[str]:
    """値の中の `{"$from": "input", "path": "/key/..."}` が指す入力の項目（先頭の段）。"""
    if isinstance(value, Mapping):
        if value.get("$from") == "input":
            path = str(value.get("path") or "")
            head = path.lstrip("/").split("/", 1)[0]
            if head:
                yield head
        for v in value.values():
            yield from _input_keys(v)
    elif isinstance(value, list | tuple):
        for v in value:
            yield from _input_keys(v)


def unobservable_inputs(ir: Mapping, allowed: set[str]) -> frozenset[str]:
    """LLM の手前で打ち切った run では見えない入力の項目。

    LLM の後ろのノード（`allowed` の外）でしか使わない入力と、出力の形（`output_schema`）を決めた LLM の指示で
    しか使わない入力（stub は指示を返さず、決まった形のダミーを返す）。どちらも、正しい IR でも打ち切った run では
    どこにも現れない。入力を使っているかは静的な検査（flow_through）が見る。
    """
    seen: set[str] = set()
    hidden: set[str] = set()
    for nid, n in _nodes(ir).items():
        keys = set(_input_keys(n.get("params") or {}))
        structured_llm = n.get("type") in LLM_NODE_TYPES and bool(
            (n.get("params") or {}).get("output_schema")
        )
        if nid not in allowed or structured_llm:
            hidden |= keys
        else:
            seen |= keys
    return frozenset(hidden - seen)


def not_delivered(
    ir: Mapping,
    expect: Expect,
    rec: CaseRecord,
    run: RunRecord,
    allowed: set[str] | None = None,
    *,
    skip: frozenset[str] = frozenset(),
) -> list[Finding]:
    sinks = _sinks(ir, rec, run, allowed)
    out = []
    for key in expect.flow_through:
        if key not in run.input or key in skip:
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


def _split_path(step_path: str) -> tuple[str, str]:
    """step_path → (要素のスコープ, ノード id)。`m[0].a` → (`m[0]`, `a`)・`a` → (``, `a`)。"""
    if "[" not in step_path:
        return "", step_path
    scope, _, node = step_path.rpartition(".")
    return scope, node


def _scoped(scope: str, node_id: str) -> str:
    return f"{scope}.{node_id}" if scope else node_id


def unexpected_skips(
    ir: Mapping,
    node_ports: Mapping[str, Sequence[str]],
    run: RunRecord,
    allowed: set[str] | None = None,
) -> list[Finding]:
    """成功した run の中の、分岐でない理由で飛ばされた step（飛ばされた上流の続きは数えない）。"""
    if run.status != "succeeded":
        return []
    by_path = {s.step_path: s for s in run.steps}
    in_edges: dict[str, list[tuple[str, str]]] = {}
    for e in _edges(ir):
        in_edges.setdefault(str(e.get("to")), []).append(
            (str(e.get("from")), str(e.get("from_port") or "out"))
        )
    memo: dict[str, bool] = {}

    def edge_ok(scope: str, src: str, port: str) -> bool:
        if port in NON_ROUTING:
            return True  # error・timeout の出口は通らないのが普通
        if len(set(node_ports.get(src) or ()) - NON_ROUTING) >= 2:
            return True  # 分岐の通らなかった側
        s = by_path.get(_scoped(scope, src))
        return s is not None and s.status == "skipped" and explained(s.step_path)

    def explained(path: str) -> bool:
        if path not in memo:
            memo[path] = True  # 循環の守り（IR は DAG のはず）
            scope, node = _split_path(path)
            memo[path] = all(edge_ok(scope, src, port) for src, port in in_edges.get(node, []))
        return memo[path]

    out = []
    for s in run.steps:
        if s.status != "skipped" or explained(s.step_path):
            continue
        scope, node = _split_path(s.step_path)
        # 元（飛ばされていない上流）だけを数える。LLM の手前に限るときは、元が手前にあるものだけ（dead edge と同じ）。
        roots = [
            (src, port)
            for src, port in in_edges.get(node, [])
            if not edge_ok(scope, src, port)
            and getattr(by_path.get(_scoped(scope, src)), "status", None) != "skipped"
            and (allowed is None or src in allowed)
        ]
        for src, port in roots:
            st = getattr(by_path.get(_scoped(scope, src)), "status", "記録なし")
            out.append(
                Finding(
                    "unexpected_skip",
                    s.node_id,
                    f"{s.step_path} が飛ばされた（{src} -[{port}]-> は分岐ではない・{src} は {st}）",
                )
            )
    return out


def unexpected_status(run: RunRecord, expected: str, last: bool) -> list[Finding]:
    """run の状態が期待と違う（succeeded は全部の run、ほかは最後の run で見る・照合子の status と同じ）。"""
    if expected == "any" or (expected != "succeeded" and not last):
        return []
    if run.status == expected:
        return []
    why = f"・{run.fail_reason}" if run.fail_reason else ""
    return [
        Finding(
            "unexpected_status", None, f"run の状態が {run.status}（期待 {expected}{why[:200]}）"
        )
    ]


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
    expect_status: str | None = None,
    skips: bool = False,
) -> DynamicGrade:
    """run_case の記録から dead edge・差し込みの残り・入力が届かない・想定外の状態と skipped を見つける。

    `restrict` は llm_dependent の run_case を stub で流したとき（最初の LLM のノードまでに限る）。
    `delivery` は「入力が届かない」を見るか（stub の run で、`expect.flow_through` が false でないとき）。`restrict`
    のときは、打ち切りで見えない入力（`unobservable_inputs`）を外して見る。
    `expect_status` は run の状態の期待（None は見ない・stub で流した llm_dependent でない run_case に渡す）。
    `skips` は想定外の skipped を見るか（期待が succeeded の run_case）。
    役割（`RunRecord.workflow`・無ければ main）ごとに IR・出口・期待を引く。
    """
    findings: list[Finding] = []
    allowed_any: set[str] | None = set() if restrict else None
    for i, run in enumerate(rec.runs):
        role = run.workflow or "main"
        ir = irs[role]
        ports = node_ports.get(role) or {}
        allowed = before_llm(ir) if restrict else None
        if allowed is not None and allowed_any is not None:
            allowed_any |= allowed
        findings += dead_edges(ir, ports, run, allowed)
        if delivery:
            hidden = unobservable_inputs(ir, allowed) if allowed is not None else frozenset()
            findings += not_delivered(ir, expects[role], rec, run, allowed, skip=hidden)
        if expect_status is not None:
            findings += unexpected_status(run, expect_status, last=i == len(rec.runs) - 1)
        if skips:
            findings += unexpected_skips(ir, ports, run, allowed)
    findings += leftovers(rec, allowed_any)
    return DynamicGrade(findings, [r.status for r in rec.runs], restrict)
