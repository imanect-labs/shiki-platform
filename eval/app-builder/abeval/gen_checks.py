"""課題の検査 G1〜G9（GAP-30 §3.4）。中核の課題集（T7）のテストと、課題の水増し生成（T11）が使う。

| 検査 | 中身 | ここ |
| --- | --- | --- |
| G1 形 | 課題のスキーマ。run_case の入力が `expect.inputs` と合う | `tasks.parse_task` |
| G2 正解が通る | 正解の IR が ir_lint で S2s ゼロ・課題の検査（2 層目）も通る・S5 も合う | ir_lint が要る |
| G3 oracle | 正解の IR を保存して run_case を流し、全部通る | サーバーが要る（tests/test_reference_it.py） |
| G4 漏れ | 依頼文と聞き返しへの答えに、ノードの種類・出口の名前・`$from`・`$template`・JSON が無い | カタログから引く |
| G5 必要な値 | run_case の入力の各項目に当たる語（`expect.inputs[].terms`）が、どの言い方の依頼文にもある | |
| G6 独立の判定 | 期待を見せない別の呼び出しで答えさせて照合する | LLM が要る（ここには無い） |
| G7 重複 | ほかの依頼文との文字 3-gram の Jaccard が 0.6 未満 | |
| G8 架空の名前 | 拒否リスト（リポジトリの外 `~/.config/shiki-dev/abeval-denylist.txt`）の語が課題のどこにも無い | 無ければ理由を出して飛ばす |
| G9 対象の機能 | `requires` が対象の版にある | 版の機能（カタログ・OpenAPI）から |
"""

from __future__ import annotations

import os
import re
import unicodedata
from collections.abc import Iterable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from abeval.grade import mutate
from abeval.grade.static import LintCatalog, LintResult, grade_static, run_ir_lint
from abeval.tasks import Task, offline_irs

DENYLIST = Path("~/.config/shiki-dev/abeval-denylist.txt")
JACCARD_LIMIT = 0.6
# IR の書き方そのもの（依頼文に出たら答えの漏れ）。
IR_TOKENS = ("$from", "$template", "from_port", "input_schema", "declared_scopes", "on_error")
_JSON = re.compile(r"[{\[]\s*\"[^\"\n]{0,64}\"\s*[:,\]]")


@dataclass
class GResult:
    check: str
    task_id: str
    ok: bool
    problems: list[str] = field(default_factory=list)
    skipped: str | None = None

    def to_dict(self) -> dict:
        return {
            "check": self.check,
            "task": self.task_id,
            "ok": self.ok,
            "problems": self.problems,
            "skipped": self.skipped,
        }


# --- G2 ---

NULL_ID = "null"


def lint_corpus(tasks: Sequence[Task]) -> list[tuple[str, dict]]:
    """ir_lint に掛ける IR の一式（G2 と採点器の自己テスト）。id は `<課題>/<役割>`・`<課題>/<役割>/mut:<壊し方>`・null。"""
    items: list[tuple[str, dict]] = []
    for t in tasks:
        for role, ir in offline_irs(t).items():
            items.append((f"{t.id}/{role}", ir))
            for m, bad in mutate.mutants(ir, t.role(role).expect):
                items.append((f"{t.id}/{role}/mut:{m.name}", bad))
    items.append((NULL_ID, mutate.null_ir("null-ir")))
    return items


def g2_reference_passes(
    tasks: Sequence[Task],
    catalog: LintCatalog,
    lint: Mapping[str, LintResult] | None = None,
    bin_path: Path | None = None,
) -> list[GResult]:
    """正解の IR（置き場所の印はダミーの UUID）を ir_lint と課題の検査に掛ける。`lint` は記録した結果（無ければ流す）。"""
    items = [(f"{t.id}/{role}", ir) for t in tasks for role, ir in offline_irs(t).items()]
    if lint is None:
        lint, _ = run_ir_lint(items, bin_path)
    out = []
    for t in tasks:
        problems = []
        for role, ir in offline_irs(t).items():
            key = f"{t.id}/{role}"
            if key not in lint:
                problems.append(f"{role}: ir_lint の結果がありません")
                continue
            g = grade_static(ir, t.role(role).expect, lint[key], catalog)
            if g.s2s:
                problems.append(
                    f"{role}: S2s {g.to_dict()['lint']} {[f.detail for f in g.task_findings]}"
                )
            if not g.s5:
                problems.append(f"{role}: S5 {[f.detail for f in g.structure]}")
        out.append(GResult("G2", t.id, not problems, problems))
    return out


# --- G4 ---


def leak_terms(catalog: LintCatalog) -> tuple[list[str], list[str]]:
    """(ノードの種類と IR の書き方, 出口の名前)。出口は英単語なので語の境で照合する。"""
    return sorted(catalog.nodes) + list(IR_TOKENS), sorted(catalog.port_names())


def g4_no_leak(task: Task, catalog: LintCatalog) -> GResult:
    exact, ports = leak_terms(catalog)
    port_re = re.compile(
        r"(?<![A-Za-z0-9_])(" + "|".join(map(re.escape, ports)) + r")(?![A-Za-z0-9_])"
    )
    problems = []
    for i, text in enumerate(task.texts()):
        where = list(task.phrasings)[i] if i < len(task.phrasings) else "clarifications"
        t = unicodedata.normalize("NFKC", text)
        for term in exact:
            if term in t:
                problems.append(f"{where}: {term!r} を含む")
        for m in port_re.finditer(t):
            problems.append(f"{where}: 出口の名前 {m.group(1)!r} を含む")
        if _JSON.search(t):
            problems.append(f"{where}: JSON の形を含む: {_JSON.search(t).group(0)!r}")
    return GResult("G4", task.id, not problems, problems)


# --- G5 ---


def g5_values_named(task: Task) -> GResult:
    problems = []
    for case in task.run_cases:
        for step in case.steps:
            expect = task.role(step.workflow).expect
            terms = {i.key: i.terms for i in expect.inputs}
            for key in step.input:
                for name, text in task.phrasings.items():
                    t = unicodedata.normalize("NFKC", text)
                    if not any(unicodedata.normalize("NFKC", w) in t for w in terms.get(key, ())):
                        problems.append(
                            f"{case.id}: 入力 {key!r} に当たる語 {list(terms.get(key, ()))} が {name} に無い"
                        )
    return GResult("G5", task.id, not problems, sorted(set(problems)))


# --- G7 ---


def trigrams(text: str) -> set[str]:
    t = re.sub(r"\s+", "", unicodedata.normalize("NFKC", text))
    return {t[i : i + 3] for i in range(max(len(t) - 2, 0))}


def jaccard(a: str, b: str) -> float:
    x, y = trigrams(a), trigrams(b)
    if not x and not y:
        return 1.0
    return len(x & y) / len(x | y)


def g7_not_duplicated(
    tasks: Sequence[Task], existing: Iterable[tuple[str, str]] = (), limit: float = JACCARD_LIMIT
) -> list[GResult]:
    """依頼文どうし（同じ課題の言い方どうしと、ほかの課題・既存の依頼文）の 3-gram の Jaccard。"""
    items = [(f"{t.id}/{k}", v) for t in tasks for k, v in t.phrasings.items()]
    others = list(existing)
    worst: dict[str, list[str]] = {t.id: [] for t in tasks}
    for i, (a_id, a) in enumerate(items):
        for b_id, b in items[i + 1 :] + others:
            j = jaccard(a, b)
            if j >= limit:
                worst[a_id.split("/")[0]].append(f"{a_id} と {b_id} の Jaccard が {j:.2f}")
    return [GResult("G7", tid, not p, p) for tid, p in worst.items()]


# --- G8 ---


def load_denylist(path: Path | None = None) -> tuple[list[str] | None, str | None]:
    """拒否リスト（1 行 1 語・# は注記）。(語, 飛ばす理由)。"""
    p = Path(os.environ.get("ABEVAL_DENYLIST") or (path or DENYLIST)).expanduser()
    if not p.is_file():
        return None, f"拒否リスト {p} が無いので G8 を飛ばします（リポジトリの外に置く・権限 600）"
    words = []
    for line in p.read_text(encoding="utf-8").splitlines():
        w = line.split("#", 1)[0].strip()
        if w:
            words.append(unicodedata.normalize("NFKC", w).casefold())
    return words, None


def _all_strings(value: Any, at: str = "") -> Iterator[tuple[str, str]]:
    """(場所, 文字)。キーも文字として見る。"""
    if isinstance(value, str):
        yield at or "/", value
    elif isinstance(value, Mapping):
        for k, v in value.items():
            yield f"{at}/{k}（キー）", str(k)
            yield from _all_strings(v, f"{at}/{k}")
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            yield from _all_strings(v, f"{at}/{i}")


def g8_fictional(task: Task, words: Sequence[str] | None, skipped: str | None = None) -> GResult:
    """課題のどこにも（依頼文・fixture・正解の IR・期待）拒否リストの語が無い。

    理由には場所と語の長さだけを書き、語そのものや前後の文は出さない（拒否リストは客先の名前を含むので、結果や
    ログに残さない）。
    """
    if words is None:
        return GResult("G8", task.id, True, [], skipped or "拒否リストがありません")
    problems = []
    for at, s in _all_strings(task.raw):
        t = unicodedata.normalize("NFKC", s).casefold()
        for w in words:
            if w in t:
                problems.append(f"{at}: 拒否リストの語を含む（{len(w)} 文字の語）")
    return GResult("G8", task.id, not problems, sorted(set(problems)))


# --- G9 ---


def version_features(
    catalog: LintCatalog,
    capabilities: Mapping[str, Any] | None = None,
    *,
    triggers: Iterable[str] = ("interactive", "schedule"),
) -> set[str]:
    """版の機能の印（`node:<種類>`・`cap:<機能>`・`trigger:<種類>`）。"""
    out = {f"node:{t}" for t, n in catalog.nodes.items() if n.available}
    out |= {f"cap:{k}" for k, v in (capabilities or {}).items() if v}
    out |= {f"trigger:{t}" for t in triggers}
    return out


def g9_supported(task: Task, features: set[str]) -> GResult:
    missing = [r for r in task.requires if r not in features]
    return GResult("G9", task.id, not missing, [f"版に無い: {r}" for r in missing])


def verify_offline(
    tasks: Sequence[Task],
    catalog: LintCatalog,
    *,
    lint: Mapping[str, LintResult] | None = None,
    with_g2: bool = True,
    denylist: Path | None = None,
    features: set[str] | None = None,
) -> list[GResult]:
    """`--verify --offline` の検査（G1 は読み込みで済み・G3 と G6 は流さない）。"""
    words, why = load_denylist(denylist)
    out: list[GResult] = []
    if with_g2:
        out += g2_reference_passes(tasks, catalog, lint)
    for t in tasks:
        out += [g4_no_leak(t, catalog), g5_values_named(t), g8_fictional(t, words, why)]
        if features is not None:
            out.append(g9_supported(t, features))
    out += g7_not_duplicated(tasks)
    return out
