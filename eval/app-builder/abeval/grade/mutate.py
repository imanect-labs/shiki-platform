"""採点器の自己テスト用の壊し方（GAP-30 §5.4）。正解の IR を機械的に壊し、期待した検査で捕まることを確かめる。

| 名前 | 壊し方 | 期待する検査 |
| --- | --- | --- |
| `edge_port_key` | エッジに `"port": "next"` を足す（無いキー） | `lint:ir.schema_violation`（V1 の誤り） |
| `from_port_next` | エッジの `from_port` を `next` にする | `lint:ir.unknown_port`（S2s）と `dyn:dead_edge`（S2d・V0 の検証は通る） |
| `escape_var` | 差し込み `{x}` を `{{x}}` にする | `lint:ir.template_escaped_var`（S2s の警告）と `dyn:leftover`（S2d） |
| `unknown_var` | vars に無い `{zz_unknown}` を足す | `lint:ir.bad_template`（S2s） |
| `from_node_id` | vars の中の `$from` を `nodes.<id>.output` からノード id にする | `lint:ir.bad_ref`（S2s） |
| `input_key` | 下流に届くべき入力の参照の key を替える | `task:input_ref`（入力の参照ミス）と `dyn:not_delivered`（S2d） |
| `drop_node_type` | 期待のノード種（all_of）のノードを抜く | `s5` |
| `llm_path_category` | 形の無い llm.invoke の出力の path を `/category` にする | `task:output_path` |
| `drop_folder` | きっかけが schedule・event の課題で、保存（storage.write・csv.write）の folder を省く | `lint:ir.write_folder_required`（自動実行をオンにできない・S2s） |

当てはまらない IR（壊す場所が無い）では None を返す。ノード id・エッジの順は保つ（記録と並べて比べるため）。
"""

from __future__ import annotations

import copy
import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any

from abeval.grade.static import LLM_NODE_TYPES, iter_node_refs, pointer_tokens
from abeval.tasks import Expect

_VAR = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)\}(?!\})")


@dataclass(frozen=True)
class Mutation:
    name: str
    expect: frozenset[str]
    dynamic: frozenset[str]
    apply: Callable[[dict, Expect], dict | None]


def _templates(value: Any) -> list[dict]:
    out = []
    if isinstance(value, dict):
        if isinstance(value.get("$template"), str) and isinstance(value.get("vars"), dict):
            out.append(value)
        for v in value.values():
            out += _templates(v)
    elif isinstance(value, list):
        for v in value:
            out += _templates(v)
    return out


def _all_templates(ir: dict) -> list[dict]:
    return [t for n in ir.get("nodes") or [] for t in _templates(n.get("params"))]


def edge_port_key(ir: dict, _: Expect) -> dict | None:
    if not ir.get("edges"):
        return None
    m = copy.deepcopy(ir)
    m["edges"][0]["port"] = "next"
    return m


def _edge_for_next(ir: dict) -> int | None:
    """`next` にしたときに出口から外れる線（error・timeout・分岐の出口を除いた最初のもの）。"""
    types = {n["id"]: n.get("type") for n in ir.get("nodes") or []}
    for i, e in enumerate(ir.get("edges") or []):
        port = e.get("from_port") or "out"
        if port == "out" and types.get(e.get("from")) not in ("control.branch", "control.switch"):
            return i
    return None


def from_port_next(ir: dict, _: Expect) -> dict | None:
    i = _edge_for_next(ir)
    if i is None:
        return None
    m = copy.deepcopy(ir)
    m["edges"][i]["from_port"] = "next"
    return m


def escape_var(ir: dict, _: Expect) -> dict | None:
    m = copy.deepcopy(ir)
    for t in _all_templates(m):
        hit = _VAR.search(t["$template"])
        if hit and hit.group(1) in t["vars"]:
            name = hit.group(1)
            t["$template"] = t["$template"].replace("{" + name + "}", "{{" + name + "}}")
            return m
    return None


def unknown_var(ir: dict, _: Expect) -> dict | None:
    m = copy.deepcopy(ir)
    ts = _all_templates(m)
    if not ts:
        return None
    ts[0]["$template"] += " {zz_unknown}"
    return m


def from_node_id(ir: dict, _: Expect) -> dict | None:
    m = copy.deepcopy(ir)
    for t in _all_templates(m):
        for v in t["vars"].values():
            src = v.get("$from") if isinstance(v, dict) else None
            if isinstance(src, str) and src.startswith("nodes.") and src.endswith(".output"):
                v["$from"] = src[len("nodes.") : -len(".output")]
                return m
    return None


def _rename_input(value: Any, old: str, new: str) -> int:
    n = 0
    if isinstance(value, dict):
        if value.get("$from") == "input":
            tokens = pointer_tokens(value.get("path"))
            if tokens and tokens[0] == old and "default" not in value:
                value["path"] = "/" + "/".join([new, *tokens[1:]])
                n += 1
        for v in value.values():
            n += _rename_input(v, old, new)
    elif isinstance(value, list):
        for v in value:
            n += _rename_input(v, old, new)
    return n


def input_key(ir: dict, expect: Expect) -> dict | None:
    """下流に届くべき入力（flow_through の先頭）の参照を、全部 `<key>_x` に替える（input_schema は残す）。"""
    if not expect.flow_through:
        return None
    key = expect.flow_through[0]
    m = copy.deepcopy(ir)
    n = sum(_rename_input(node.get("params"), key, f"{key}_x") for node in m.get("nodes") or [])
    return m if n else None


def drop_node_type(ir: dict, expect: Expect) -> dict | None:
    """all_of の種類のノードを全部抜く（そのノードにつながる線と、その子も抜く）。"""
    for t in expect.node_types.get("all_of") or []:
        drop = {n["id"] for n in ir.get("nodes") or [] if n.get("type") == t}
        if not drop:
            continue
        m = copy.deepcopy(ir)
        drop |= {n["id"] for n in m["nodes"] if n.get("parent") in drop}
        m["nodes"] = [n for n in m["nodes"] if n["id"] not in drop]
        m["edges"] = [
            e for e in m.get("edges") or [] if e.get("from") not in drop and e.get("to") not in drop
        ]
        return m if m["nodes"] else None
    return None


def llm_path_category(ir: dict, _: Expect) -> dict | None:
    nodes = {n["id"]: n for n in ir.get("nodes") or []}
    targets = {
        nid
        for nid, n in nodes.items()
        if n.get("type") == "llm.invoke" and not (n.get("params") or {}).get("output_schema")
    }
    m = copy.deepcopy(ir)
    for _, _, ref in iter_node_refs(m):
        src = ref["$from"]
        if src.startswith("nodes.") and src[len("nodes.") : -len(".output")] in targets:
            ref["path"] = "/category"
            return m
    return None


def drop_folder(ir: dict, expect: Expect) -> dict | None:
    """自動で動く課題の、最初の保存のノードの folder を省く（stub の対話の実行はルートに書けて通る）。"""
    if (expect.trigger or {}).get("kind") not in ("schedule", "event"):
        return None
    m = copy.deepcopy(ir)
    for n in m.get("nodes") or []:
        params = n.get("params")
        if (
            n.get("type") in ("storage.write", "csv.write")
            and isinstance(params, dict)
            and params.pop("folder", None) is not None
        ):
            return m
    return None


MUTATIONS: tuple[Mutation, ...] = (
    Mutation("edge_port_key", frozenset({"lint:ir.schema_violation"}), frozenset(), edge_port_key),
    Mutation(
        "from_port_next",
        frozenset({"lint:ir.unknown_port"}),
        frozenset({"dyn:dead_edge"}),
        from_port_next,
    ),
    Mutation(
        "escape_var",
        frozenset({"lint:ir.template_escaped_var"}),
        frozenset({"dyn:leftover"}),
        escape_var,
    ),
    Mutation("unknown_var", frozenset({"lint:ir.bad_template"}), frozenset(), unknown_var),
    Mutation("from_node_id", frozenset({"lint:ir.bad_ref"}), frozenset(), from_node_id),
    Mutation(
        "input_key", frozenset({"task:input_ref"}), frozenset({"dyn:not_delivered"}), input_key
    ),
    Mutation("drop_node_type", frozenset({"s5"}), frozenset(), drop_node_type),
    Mutation("llm_path_category", frozenset({"task:output_path"}), frozenset(), llm_path_category),
    Mutation("drop_folder", frozenset({"lint:ir.write_folder_required"}), frozenset(), drop_folder),
)


def by_name(name: str) -> Mutation:
    for m in MUTATIONS:
        if m.name == name:
            return m
    raise KeyError(name)


def mutants(ir: Mapping, expect: Expect) -> list[tuple[Mutation, dict]]:
    """当てはまる壊し方と、壊した IR。"""
    out = []
    for m in MUTATIONS:
        got = m.apply(copy.deepcopy(dict(ir)), expect)
        if got is not None:
            out.append((m, got))
    return out


def null_ir(name: str) -> dict:
    """中身の無い IR（stub の `emitwf:ok` と同じ形・1 ノード・線なし）。どの課題の照合も通らないはず。"""
    return {
        "ir_version": 1,
        "name": name,
        "display_name": "スタブフロー",
        "declared_scopes": [],
        "triggers": [{"kind": "interactive"}],
        "nodes": [
            {
                "id": "compute",
                "type": "script.run",
                "params": {"source": {"inline": "function main(i){ return { ok: true }; }"}},
            }
        ],
        "edges": [],
    }


def has_llm(ir: Mapping) -> bool:
    return any(n.get("type") in LLM_NODE_TYPES for n in ir.get("nodes") or [])
