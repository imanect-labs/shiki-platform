"""Mode A・Mode B が共有する試行の部品（試行の id・依頼文の参考情報・IR の id の付け替え・役割・聞き返し）。

- 試行（課題 × 言い方 × 試行の番号 × 条件）の id は abeval.proxy の id の形（英数字と . _ : -）にする
  （会話 ID と active で費用を試行に割り当てるため）。
- 生成した IR は、fixture の id を置き場所の印（`@table:<ref>` など）に戻してから記録する（`canonical`）。
  実験のたびに fixture を作り直すので id が変わっても、記録から採点し直せる（印を今の id に戻す・`concrete`）。
  ir_lint には、ref ごとに決まったダミーの UUID にして渡す（`offline`・結果が実行ごとに変わらない）。
- 依頼文の「参考情報」は、fixture のテーブル（表示名・id・列の key と型）・フォルダー・ファイルの id と、
  入力の項目（課題の入力の key・依頼文での呼び名・型）を載せる（Mode A にはデータを調べるツールが無い。
  Mode B のチャットにもフォルダーやファイルの id を調べる道具が無い。製品の検証は名指しするテーブルやフォルダーの
  実在を見る。run_case は課題の key で入力を送るので、key を知らせないと名前を当てる運で合否が決まる）。
  Mode A と Mode B に同じ参考情報を添える。キャッシュのキーには印のままの文を使う（id が変わってもキーが
  変わらない）。
"""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from abeval.tasks import FIXTURE_KIND, PLACEHOLDER, Task, dummy_ids

MODEL = "claude-haiku-5-5"
STUB_MODEL = "default"
EMIT_TOOL = "emit_workflow"
_SAFE = re.compile(r"[^A-Za-z0-9._:-]+")
# 聞き返しの判定（単純な規則）: 最後の文が「？」で終わる。課題に `ask_terms` があればその語でも判定する。
_TRAILING = " \t\r\n*_`>」』）)】]"


@dataclass(frozen=True)
class TrialSpec:
    """1 試行の条件。"""

    exp: str
    mode: str  # a | b
    label: str
    task_id: str
    phrasing: str
    n: int
    effort: str | None = None
    max_steps: int | None = None

    @property
    def trial_id(self) -> str:
        # effort の指定が無いとき、Mode B は製品の Auto（auto）、Mode A は思考なし（nothink）。
        parts = [
            self.task_id,
            self.phrasing,
            str(self.n),
            self.effort or ("auto" if self.mode == "b" else "nothink"),
        ]
        if self.max_steps is not None:
            parts.append(f"s{self.max_steps}")
        return _SAFE.sub("-", ".".join(parts))[:128]

    def condition(self) -> dict:
        return {"effort": self.effort, "max_steps": self.max_steps}

    def to_dict(self) -> dict:
        return {
            "exp": self.exp,
            "mode": self.mode,
            "label": self.label,
            "task": self.task_id,
            "phrasing": self.phrasing,
            "n": self.n,
            "effort": self.effort,
            "max_steps": self.max_steps,
            "trial_id": self.trial_id,
        }


def is_question(text: str | None, terms: Sequence[str] = ()) -> bool:
    """最後の文章が聞き返しか（「？」か「?」で終わる・課題の語を含む）。"""
    if not text:
        return False
    body = text.rstrip(_TRAILING)
    if body.endswith(("？", "?")):
        return True
    return any(t and t in text for t in terms)


def ask_terms(task: Task) -> tuple[str, ...]:
    raw = task.raw.get("ask_terms") if isinstance(task.raw, Mapping) else None
    return tuple(t for t in raw or () if isinstance(t, str) and t)


# --- fixture の id と置き場所の印 ---


def placeholder_ids(task: Task) -> dict[str, str]:
    """ref → 置き場所の印（キャッシュのキーの依頼文に使う）。"""
    return {ref: f"@{kind}:{ref}" for ref, kind in task.fixture_refs().items()}


def canonical(value: Any, refs: Mapping[str, str], task: Task) -> Any:
    """fixture の id（文字列の全体が一致するもの）を置き場所の印に戻した複製。"""
    kinds = task.fixture_refs()
    back = {v: f"@{kinds[ref]}:{ref}" for ref, v in refs.items() if ref in kinds}
    return _replace(value, back)


def _replace(value: Any, table: Mapping[str, str]) -> Any:
    if isinstance(value, Mapping):
        return {k: _replace(v, table) for k, v in value.items()}
    if isinstance(value, list):
        return [_replace(v, table) for v in value]
    if isinstance(value, str) and value in table:
        return table[value]
    return value


def _known(value: Any, ids: Mapping[str, str], kinds: Mapping[str, str]) -> Any:
    """置き場所の印のうち、課題の fixture にあるもの（種類も合うもの）だけを置き換える。

    課題の正解は `tasks.substitute`（知らない印は誤り）で置き換えるが、AI が書いた IR に印の形の文字が入っていても
    採点を止めないよう、ここでは知らない印をそのまま残す（製品の検証が誤りにする）。
    """
    if isinstance(value, Mapping):
        return {k: _known(v, ids, kinds) for k, v in value.items()}
    if isinstance(value, list):
        return [_known(v, ids, kinds) for v in value]
    if isinstance(value, str):
        m = PLACEHOLDER.match(value)
        if m and kinds.get(m.group(2)) == m.group(1) and m.group(2) in ids:
            return ids[m.group(2)]
    return value


def concrete(value: Any, refs: Mapping[str, str], task: Task) -> Any:
    """置き場所の印を今の fixture の id にした複製。"""
    return _known(value, refs, task.fixture_refs())


def offline(value: Any, task: Task) -> Any:
    """置き場所の印をダミーの UUID にした複製（ir_lint に掛ける形・実行ごとに同じ）。"""
    return _known(value, dummy_ids(task), task.fixture_refs())


def _field_line(f: Mapping) -> str:
    bits = [str(f.get("type") or "?")]
    if f.get("display_name"):
        bits.append(str(f["display_name"]))
    opts = f.get("options")
    if isinstance(opts, Mapping):
        opts = opts.get("choices")
    if isinstance(opts, list) and opts:
        names = [str(c.get("value", c)) if isinstance(c, Mapping) else str(c) for c in opts]
        bits.append("選択肢 " + "・".join(names))
    return f"{f.get('key')}（{'・'.join(bits)}）"


_TRIGGER_WORDS = {
    "interactive": "手で動かすワークフロー",
    "schedule": "決まった時刻に動くワークフロー",
    "event": "ファイルが置かれたときに動くワークフロー",
}


def _type_word(value: Any) -> str:
    if isinstance(value, bool):
        return "真偽"
    if isinstance(value, int | float):
        return "数値"
    if isinstance(value, list):
        inner = {_type_word(v) for v in value}
        return f"{inner.pop()}の配列" if len(inner) == 1 else "配列"
    if isinstance(value, Mapping):
        return "オブジェクト"
    return "文字"


def input_lines(task: Task) -> list[str]:
    """入力の項目の行（役割ごと・入力の無い役割は出さない）。型は run_case の値から読む。"""
    out = []
    for role in task.roles:
        if not role.expect.inputs:
            continue
        seen: dict[str, Any] = {}
        for case in task.run_cases:
            for step in case.steps:
                if (step.workflow or "main") == role.role:
                    for k, v in step.input.items():
                        seen.setdefault(k, v)
        items = []
        for spec in role.expect.inputs:
            bits = [spec.terms[0], _type_word(seen[spec.key]) if spec.key in seen else "文字"]
            if not spec.required:
                bits.append("任意")
            items.append(f"{spec.key}（{'・'.join(bits)}）")
        who = (
            "入力" if task.kind != "workflow_set" else f"{_TRIGGER_WORDS[role.trigger_kind]}の入力"
        )
        out.append(f"- {who}の項目（key はこのとおりにする）: {'、'.join(items)}")
    return out


def reference_info(task: Task, ids: Mapping[str, str]) -> str:
    """依頼文に添える参考情報（fixture も入力も無ければ空）。`ids` は ref → id（キーでは印）。"""
    fx = task.fixtures
    lines: list[str] = input_lines(task)
    for t in fx.get(FIXTURE_KIND["table"]) or []:
        fields = (t.get("schema") or {}).get("fields") or []
        cols = ", ".join(_field_line(f) for f in fields if isinstance(f, Mapping))
        lines.append(f"- テーブル「{t['name']}」（id: {ids[t['ref']]}）: 列 {cols}")
    for f in fx.get(FIXTURE_KIND["folder"]) or []:
        lines.append(f"- フォルダー「{f['name']}」（id: {ids[f['ref']]}）")
    for c in fx.get(FIXTURE_KIND["file"]) or []:
        where = ""
        if c.get("folder"):
            parent = next((f for f in fx.get("folders") or [] if f.get("ref") == c["folder"]), None)
            if parent:
                where = f"・フォルダー「{parent['name']}」の中"
        lines.append(f"- ファイル「{c['name']}」（id: {ids[c['ref']]}{where}）")
    if not lines:
        return ""
    return (
        "参考情報（テーブル・フォルダー・ファイルはこの環境にあるもので、id はそのまま使えます）:\n"
        + "\n".join(lines)
    )


def prompt_text(task: Task, phrasing: str, ids: Mapping[str, str], *, with_reference: bool) -> str:
    text = task.phrasings[phrasing]
    info = reference_info(task, ids) if with_reference else ""
    return f"{text}\n\n{info}" if info else text


def fixture_names(task: Task) -> dict[str, list[str]]:
    return {
        key: sorted(str(x.get("name")) for x in task.fixtures.get(key) or [])
        for key in FIXTURE_KIND.values()
    }


# --- 役割 ---


def trigger_kinds(ir: Mapping) -> list[str]:
    kinds = [t.get("kind") for t in ir.get("triggers") or [] if isinstance(t, Mapping)]
    return [k for k in kinds if isinstance(k, str)] or ["interactive"]


def assign_roles(
    task: Task, irs: Sequence[Mapping], *, prefer: str = "last"
) -> tuple[dict[str, dict], list[str]]:
    """IR（新しいものが後ろ）を課題の役割に割り当てる。(役割 → IR, 割り当たらない役割)。

    `workflow` の課題は 1 つを main にする。`workflow_set` はきっかけの種類で割り当てる。同じ役割の候補が
    いくつもあれば `prefer` で選ぶ（Mode B は最後＝最新の版、Mode A は最初＝1 回目の IR）。
    """
    if prefer not in ("first", "last"):
        raise ValueError(f"prefer は first か last です: {prefer!r}")
    irs = [dict(ir) for ir in irs if isinstance(ir, Mapping) and isinstance(ir.get("nodes"), list)]
    pick = 0 if prefer == "first" else -1
    if task.kind != "workflow_set":
        return ({"main": irs[pick]}, []) if irs else ({}, ["main"])
    out: dict[str, dict] = {}
    for role in task.roles:
        hit = [ir for ir in irs if role.trigger_kind in trigger_kinds(ir)]
        if hit:
            out[role.role] = hit[pick]
    return out, [r.role for r in task.roles if r.role not in out]


def emitted_irs(tool_uses: Sequence[Mapping]) -> list[dict]:
    """emit_workflow の呼び出しの入力から IR を取り出す（形の違うものは飛ばす）。"""
    out = []
    for tu in tool_uses:
        inp = tu.get("input")
        ir = inp.get("ir") if isinstance(inp, Mapping) else None
        if isinstance(ir, Mapping):
            out.append(dict(ir))
    return out
