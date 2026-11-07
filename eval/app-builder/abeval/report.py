"""レポート（`results/<exp>.json`・`results/errors-<exp>.jsonl`・`results/summary.md`・GAP-30 §8）。

- `results/<exp>.json`: experiment・created_at・model・実行（runs）・versions（ラベル → commit・バイナリ・設定・
  取り込みの sha・指紋）・grader・conditions・trials（採点した記録・保存された IR）・aggregates・cost。
  同じ実験をもう一度流したら、試行はキーで置き換えて足す（キャッシュから採点し直した分も入る）。
- `results/errors-<exp>.jsonl`: 基盤の失敗（採点しない）。
- `results/summary.md`: 6 節（見出しの表・失敗の内訳・課題ごとの表・effort と手数・費用・限界）。見出しの表には
  条件ごとの課題の数と、外した課題（対象外と、errors の種類ごとの採点しなかった試行）を出す。
- 書く前に検査する（`check_publishable`）: 会話の全文と要求の本文の項目（transcript・messages・response・
  thinking など）・鍵の形の値（`sk-ant-`）・拒否リスト（G8・リポジトリの外）の語。見つかれば書かない。
  拒否リストの語そのものは理由に出さない（場所と語の長さだけ）。
"""

from __future__ import annotations

import datetime as dt
import json
import os
import unicodedata
from collections.abc import Iterator, Mapping, Sequence
from pathlib import Path
from typing import Any

from abeval.config import ROOT
from abeval.gen_checks import load_denylist
from abeval.metrics import (
    aggregate,
    condition_name,
    condition_of,
    dedupe,
    excluded,
    failure_breakdown,
    paired_bootstrap,
    task_means,
    trial_identity,
)
from abeval.trial import MODEL

RESULTS_DIR = ROOT / "results"


def results_dir() -> Path:
    """結果の置き場（`ABEVAL_RESULTS_DIR` で差し替えられる・テストは一時の場所に向ける）。"""
    env = os.environ.get("ABEVAL_RESULTS_DIR")
    return Path(env).expanduser() if env else RESULTS_DIR


# 結果に入れない項目（会話の全文・要求と応答の本文）。IR（irs の下）の中は製品の形なので見ない。
FORBIDDEN_KEYS = frozenset(
    {"transcript", "messages", "response", "thinking", "request_body", "raw_request", "final_text"}
)
IR_KEYS = frozenset({"irs", "ir"})
KEY_MARKER = "sk-ant-"


class Unpublishable(ValueError):
    """結果に入れてはいけないものがある（`problems` に場所と理由）。"""

    def __init__(self, problems: list[str]):
        super().__init__("結果に入れてはいけないものがあります:\n- " + "\n- ".join(problems[:20]))
        self.problems = problems


def _walk(value: Any, at: str, in_ir: bool) -> Iterator[tuple[str, str | None, Any, bool]]:
    """(場所, キー, 値, IR の中か)。"""
    if isinstance(value, Mapping):
        for k, v in value.items():
            here = f"{at}/{k}"
            yield here, str(k), v, in_ir
            yield from _walk(v, here, in_ir or str(k) in IR_KEYS)
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            yield from _walk(v, f"{at}/{i}", in_ir)


def check_publishable(
    doc: Any, words: Sequence[str] | None, *, forbidden_keys: bool = True
) -> list[str]:
    """結果に入れてよいか。問題の場所と理由（語そのものは出さない）の列。

    `forbidden_keys=False` は会話の全文の項目名の検査をしない（取り込んだ製品のツールの定義には、`messages` などの
    名前の引数があり得るため。鍵の形と拒否リストの語は見る）。
    """
    problems = []
    norm_words = [unicodedata.normalize("NFKC", w).casefold() for w in words or [] if w]

    def strings(at: str, s: str) -> None:
        if KEY_MARKER in s.lower():
            problems.append(f"{at}: 鍵の形の値")
        t = unicodedata.normalize("NFKC", s).casefold()
        for w in norm_words:
            if w in t:
                problems.append(f"{at}: 拒否リストの語を含む（{len(w)} 文字の語）")

    if isinstance(doc, str):
        strings("/", doc)
    for at, key, value, in_ir in _walk(doc, "", False):
        if key is not None:
            if forbidden_keys and not in_ir and key in FORBIDDEN_KEYS:
                problems.append(f"{at}: 会話の全文・本文の項目（{key}）は結果に入れない")
            strings(f"{at}（キー）", key)
        if isinstance(value, str):
            strings(at, value)
    return sorted(set(problems))


def _require_publishable(doc: Any, words: Sequence[str] | None) -> None:
    problems = check_publishable(doc, words)
    if problems:
        raise Unpublishable(problems)


def _atomic_write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)


def result_path(exp: str, where: Path | None = None) -> Path:
    return Path(where or results_dir()) / f"{exp}.json"


def read_result(exp: str, where: Path | None = None) -> dict | None:
    p = result_path(exp, where)
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None


def read_errors(exp: str, where: Path | None = None) -> list[dict]:
    """results/errors-<exp>.jsonl（基盤の失敗・無ければ空。読めない行は飛ばす）。"""
    p = Path(where or results_dir()) / f"errors-{exp}.jsonl"
    try:
        lines = p.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return []
    out = []
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if isinstance(e, dict):
            out.append(e)
    return out


def merge_result(prev: Mapping | None, new: Mapping) -> dict:
    """同じ実験の前の結果に足す（試行は試行の名前で置き換える・実行と版は足す）。

    採点器の版（`grader`）が前と違うときは、前の試行を捨てて `merge_notes` に残す。違う規則で採点した試行を
    同じ表で数えないため（BUG-254）。捨てた試行は、同じ条件で流し直す（Mode A は生成をキャッシュから読み直して
    採点し直す・Mode B は採点器の版がキャッシュのキーに入るので会話から流し直す）。
    """
    if not prev:
        return dict(new)
    prev_trials = list(prev.get("trials") or [])
    notes = list(prev.get("merge_notes") or [])
    if prev_trials and prev.get("grader") != new.get("grader"):
        notes.append(
            {
                "dropped_trials": len(prev_trials),
                "reason": "採点器の版が違う",
                "prev_grader": prev.get("grader"),
                "grader": new.get("grader"),
            }
        )
        prev_trials = []
    trials = {trial_identity(t): t for t in prev_trials}
    trials.update({trial_identity(t): t for t in new.get("trials") or []})
    runs = [*(prev.get("runs") or []), *(new.get("runs") or [])]
    versions = {**(prev.get("versions") or {}), **(new.get("versions") or {})}
    out_of_scope = {
        (o.get("label"), o.get("task")): o
        for o in [*(prev.get("out_of_scope") or []), *(new.get("out_of_scope") or [])]
    }
    return {
        **prev,
        **new,
        "trials": list(trials.values()),
        "runs": runs,
        "versions": versions,
        "out_of_scope": list(out_of_scope.values()),
        "merge_notes": notes,
    }


def build_result(
    *,
    exp: str,
    trials: Sequence[Mapping],
    runs: Sequence[Mapping],
    versions: Mapping[str, Mapping],
    grader: Mapping,
    cost: Mapping | None = None,
    out_of_scope: Sequence[Mapping] = (),
    created_at: str | None = None,
) -> dict:
    return {
        "experiment": exp,
        "created_at": created_at or dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
        "model": MODEL,
        "runs": list(runs),
        "versions": dict(versions),
        "grader": dict(grader),
        "conditions": sorted({condition_name(_cond(t)) for t in trials}),
        "trials": list(trials),
        "out_of_scope": list(out_of_scope),
        "cost": dict(cost or {}),
    }


def _cond(t: Mapping) -> tuple:
    return condition_of(t)


def finalize(doc: Mapping, *, iters: int = 10_000) -> dict:
    out = dict(doc)
    out["conditions"] = sorted({condition_name(_cond(t)) for t in doc.get("trials") or []})
    out["aggregates"] = aggregate(doc.get("trials") or [], iters=iters)
    out["failures"] = failure_breakdown(doc.get("trials") or [])
    return out


def write_result(
    doc: Mapping,
    errors: Sequence[Mapping],
    *,
    where: Path | None = None,
    denylist: Path | None = None,
    merge: bool = True,
    iters: int = 10_000,
) -> tuple[Path, Path]:
    """results/<exp>.json と errors-<exp>.jsonl を書く（検査に通らなければ何も書かない）。"""
    words, _ = load_denylist(denylist)
    exp = str(doc["experiment"])
    where = Path(where or results_dir())
    prev = read_result(exp, where) if merge else None
    final = finalize(merge_result(prev, doc), iters=iters)
    err_path = where / f"errors-{exp}.jsonl"
    _require_publishable(final, words)
    _require_publishable(list(errors), words)
    _atomic_write(result_path(exp, where), json.dumps(final, ensure_ascii=False, indent=1) + "\n")
    if errors or not err_path.exists():
        old = err_path.read_text(encoding="utf-8") if (merge and err_path.exists()) else ""
        lines = "".join(json.dumps(e, ensure_ascii=False, sort_keys=True) + "\n" for e in errors)
        _atomic_write(err_path, old + lines)
    return result_path(exp, where), err_path


# --- summary.md ---


def _pct(r: Mapping | None) -> str:
    if not r or r.get("rate") is None:
        return "—"
    return f"{r['rate'] * 100:.0f}%（{r['true']}/{r['n']}）"


def _ci(c: Mapping | None) -> str:
    if not c or c.get("mean") is None:
        return "—"
    return f"{c['mean'] * 100:.0f}%［{c['lo'] * 100:.0f}〜{c['hi'] * 100:.0f}］"


def _usd(v: float | None) -> str:
    return "—" if v is None else f"${v:.4f}"


def _table(head: list[str], rows: list[list[str]]) -> list[str]:
    out = ["| " + " | ".join(head) + " |", "| " + " | ".join("---" for _ in head) + " |"]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    return out


def _comparisons(trials: Sequence[Mapping], iters: int) -> list[list[str]]:
    """同じモードで条件を 1 つ目と比べる（課題を単位にした対応のあるブートストラップ）。"""
    by_cond: dict[tuple, list[Mapping]] = {}
    for t in trials:
        by_cond.setdefault(_cond(t), []).append(t)
    rows = []
    for mode in sorted({c[1] for c in by_cond}):
        conds = sorted((c for c in by_cond if c[1] == mode), key=lambda c: tuple(str(x) for x in c))
        if len(conds) < 2:
            continue
        base = task_means(by_cond[conds[0]])
        for c in conds[1:]:
            d = paired_bootstrap(base, task_means(by_cond[c]), iters=iters)
            if d["diff"] is None:
                continue
            rows.append(
                [
                    f"{condition_name(c)} − {condition_name(conds[0])}",
                    f"{d['diff'] * 100:+.0f} pt",
                    f"［{d['lo'] * 100:+.0f}〜{d['hi'] * 100:+.0f}］",
                    str(d["n_tasks"]),
                    "差がある" if d["significant"] else "決着しない（区間が 0 をまたぐ）",
                ]
            )
    return rows


LIMITS = [
    "生成と評価が同じモデル（Claude Haiku 5.5）。中核の課題は手で書き、生成した課題は分けて集計する。",
    "このマシンで外した機能: RAG・agent.invoke・Office・ai.decide（評価の設定に判定用のモデルが無い）。",
    "1 試行の費用の見積もりは推測（E0 の実測で置き換える）。",
    "製品は手の間で思考ブロックを送り返さない（anthropic.rs）。説明文の外の理由で Mode B の品質が下がり得る。",
    "温度で決定性を担保できないので、課題あたり数回の試行の幅で見る。20 課題 × 3 試行で見分けられる差は 15〜20 ポイントまで。",
    "聞き返しの判定は単純な規則（「？」で終わる・課題の語）なので取りこぼしがある（S8 の「判定できない」に残す）。",
    "1 発話のトークンの上限（30 万）と、手数の上限（既定 6）が効く条件で測っている。",
]


def _excluded_cell(kinds: Mapping[str, Mapping] | None, out_of_scope: int) -> str:
    parts = [f"対象外 {out_of_scope} 課題"] if out_of_scope else []
    parts += [f"{k} {v['tasks']} 課題（{v['trials']} 試行）" for k, v in (kinds or {}).items()]
    return "・".join(parts) or "なし"


def _headline_rows(
    docs: Sequence[Mapping], trials: Sequence[Mapping], errors: Sequence[Mapping], iters: int
) -> tuple[list[dict], list[list[str]]]:
    """見出しの表の行（条件ごとの課題の数と、外した課題の数〔対象外と errors の種類ごと〕を添える）。"""
    rows = aggregate(trials, iters=iters)
    ex = excluded(trials, errors)
    oos: dict[str, set[str]] = {}
    for d in docs:
        for o in d.get("out_of_scope") or []:
            oos.setdefault(str(o.get("label")), set()).add(str(o.get("task")))
    out = []
    seen = set()
    for r in rows:
        cond = (r["label"], r["mode"], r["effort"], r["max_steps"], False)
        seen.add(cond)
        out.append(
            [
                r["condition"],
                str(r["tasks"]),
                str(r["trials"]),
                _pct(r["s1"]),
                _pct(r["s2s"]),
                _pct(r["s2d"]),
                _pct(r.get("s4")),
                _ci(r["headline_ci"]),
                _usd(r["cost_per_completed_task_usd"]),
                _excluded_cell(ex.get(cond), len(oos.get(str(r["label"]), ()))),
            ]
        )
    # 採点した試行が 1 つも無い（全部外した）条件も出す。
    for cond in sorted((c for c in ex if c not in seen), key=lambda c: tuple(str(x) for x in c)):
        out.append(
            [condition_name(cond), "0", "0", *(["—"] * 6)]
            + [_excluded_cell(ex[cond], len(oos.get(str(cond[0]), ())))]
        )
    return rows, out


def _grader_name(d: Mapping) -> str:
    g = d.get("grader") or {}
    return f"採点器の版 {g.get('grader')}" if isinstance(g, Mapping) else "採点器の版 ?"


def same_grader(docs: Sequence[Mapping]) -> tuple[list[Mapping], list[Mapping]]:
    """いちばん新しい採点器の版（版の番号が大きく、同じなら ir_lint の sha と形の版も同じもの）で採点した実験と、
    それ以外（表から外す）。"""
    if not docs:
        return [], []

    def ident(d: Mapping) -> str:
        return json.dumps(d.get("grader") or {}, sort_keys=True)

    def number(d: Mapping) -> int:
        g = d.get("grader") or {}
        v = g.get("grader") if isinstance(g, Mapping) else None
        return v if isinstance(v, int) else -1

    best = max(docs, key=lambda d: (number(d), sum(ident(x) == ident(d) for x in docs)))
    keep = [d for d in docs if ident(d) == ident(best)]
    return keep, [d for d in docs if ident(d) != ident(best)]


def summary_md(
    docs: Sequence[Mapping],
    *,
    budget: Mapping | None = None,
    iters: int = 10_000,
    errors: Sequence[Mapping] = (),
) -> str:
    """results/<exp>.json（finalize 済み）の列と、その errors の行から summary.md を作る。

    試行は実験をまたいでキーで重ね（同じ試行を二重に数えない）、stub の試行（`run --stub`）は除く。見出しの表には
    条件ごとの課題の数と、外した課題（版に無い機能の対象外と、errors の種類ごと・採点しなかった試行）を出す
    （モードや条件で課題の集合が違うまま見出しを並べて読まないため）。
    """
    docs, left_out = same_grader(docs)
    trials = dedupe(t for d in docs for t in d.get("trials") or [])
    rows, head_rows = _headline_rows(docs, trials, errors, iters)
    lines = ["# AI によるワークフロー作成の評価（GAP-30）", ""]
    lines.append(
        "実験: "
        + ", ".join(f"{d['experiment']}（{len(d.get('trials') or [])} 試行）" for d in docs)
        + f"。モデル {MODEL}。"
    )
    if left_out:
        lines += [
            "",
            "採点器の版が違うので外した実験: "
            + "・".join(f"{d['experiment']}（{_grader_name(d)}）" for d in left_out)
            + f"。表は {_grader_name(docs[0])} で採点した実験だけ（違う規則で採点した試行を同じ表で数えない）。"
            "同じ条件で流し直すか、`abeval grade` で採点し直す。",
        ]
    lines += ["", "## 1. 見出し", ""]
    lines += _table(
        [
            "条件",
            "課題",
            "試行",
            "S1",
            "S2s",
            "S2d",
            "S4",
            "見出し（課題の 95% 区間）",
            "完了した課題あたりの費用",
            "外した課題（採点しなかった試行）",
        ],
        head_rows,
    )
    lines += [
        "",
        "見出し: Mode A は S1 ∧ ¬S2s ∧ ¬S2d、Mode B は S1 ∧ ¬S2s ∧ ¬S2d ∧ S4。",
        "",
        "「課題」は見出しの分母に入った課題の数。条件ごとに課題の集合が違うことがある（版に無い機能の対象外・"
        "予定の週に流さない date_window・基盤の失敗で採点しなかった試行）。モードや条件の見出しを並べて読むときは、"
        "課題の数と外した課題を見る。下の比べ方は同じモードの中の共通の課題だけを使う。",
    ]
    comp = _comparisons(trials, iters)
    if comp:
        lines += [""]
        lines += _table(["比べたもの", "差", "95% 区間", "課題", "判定"], comp)
    fb = failure_breakdown(trials)
    lines += ["", "## 2. 失敗の内訳", ""]
    for name, title in (
        ("validation", "検証エラーのコード"),
        ("lint", "ir_lint（S2s）"),
        ("task", "課題の検査"),
        ("dynamic", "動的な検査（S2d）"),
        ("steps_stub", "stub の配管で落ちた step の誤りのコード"),
        ("steps_by_node_stub", "stub の配管で落ちた step（ノードの種類/コード）"),
        ("steps_runs", "実モデルの run_case（Mode B）で落ちた step の誤りのコード"),
        ("steps_by_node_runs", "実モデルの run_case（Mode B）で落ちた step（ノードの種類/コード）"),
    ):
        items = ", ".join(f"`{k}` {v}" for k, v in list(fb[name].items())[:15]) or "なし"
        lines.append(f"- {title}: {items}")
    for r in rows:
        extra = r.get("status") or {}
        if extra:
            lines.append(
                f"- 生成の結果（{r['condition']}）: "
                + ", ".join(f"{k} {v}" for k, v in extra.items())
            )
    lines += ["", "## 3. 課題ごと", ""]
    conds = [r["condition"] for r in rows]
    per: dict[str, dict[str, str]] = {}
    for r_cond in sorted({_cond(t) for t in trials}, key=lambda c: tuple(str(x) for x in c)):
        name = condition_name(r_cond)
        for t in trials:
            if _cond(t) != r_cond:
                continue
            cell = per.setdefault(t["task"], {}).setdefault(name, "0/0")
            c, n = (int(x) for x in cell.split("/"))
            per[t["task"]][name] = f"{c + (1 if t.get('headline') else 0)}/{n + 1}"
    lines += _table(
        ["課題", *conds], [[task, *(per[task].get(c, "—") for c in conds)] for task in sorted(per)]
    )
    lines += ["", "## 4. effort と手数", ""]
    lines += _table(
        ["条件", "見出し", "pass@1", "費用（合計）", "平均の時間（秒）", "再試行"],
        [
            [
                r["condition"],
                _pct(r["headline"]),
                "—" if r["pass_at_k"].get("1") is None else f"{r['pass_at_k']['1'] * 100:.0f}%",
                _usd(r["cost_usd"]),
                "—" if r["mean_secs"] is None else f"{r['mean_secs']:.1f}",
                str(r["retries"]),
            ]
            for r in rows
        ],
    )
    b_rows = [r for r in rows if r["mode"] == "b"]
    if b_rows:
        lines += [""]
        lines += _table(
            [
                "条件（Mode B）",
                "emit の回数",
                "検証エラー",
                "手数",
                "上限に達した",
                "emit を読めた",
                "承認の要求（却下）",
                "重大",
            ],
            [
                [
                    r["condition"],
                    f"{r['s6']['emit_count'] or 0:.1f}",
                    f"{r['s6']['validation_errors'] or 0:.1f}",
                    f"{r['s6']['steps'] or 0:.1f}",
                    _pct(r["s6"]["hit_step_limit"]),
                    _pct(r["s6"]["emit_loaded"]),
                    f"{r['s9']['requests']}（{r['s9']['denied']}）",
                    str(r["s9"]["serious"]),
                ]
                for r in b_rows
            ],
        )
    lines += ["", "## 5. 費用", ""]
    by_exp = (budget or {}).get("by_exp_usd") or {}
    for d in [*docs, *left_out]:
        exp = str(d["experiment"])
        tried = sum(r["cost_usd"] for r in aggregate(d.get("trials") or [], iters=1))
        lines.append(
            f"- {exp}: 試行の分 {_usd(tried)}"
            + (f"・評価の台帳の累計 {_usd(by_exp.get(exp))}" if exp in by_exp else "")
        )
    lines.append(
        "- 試行の分は、試行に割り当てた費用（キャッシュから流し直した試行と `--regrade-from` で採点し直した試行は、"
        "もとの生成の費用を数える）。評価の台帳の累計は、その実験の名前で実際に払った額（流し直した分も含む）。"
    )
    if budget:
        lines.append(
            f"- 評価の累計 {_usd(budget.get('eval_usd'))} / ${budget.get('eval_cap_usd')}"
            f"（残り {_usd(budget.get('eval_left_usd'))}）"
        )
        if budget.get("relay_usd") is not None:
            lines.append(
                f"- 総額（手元の中継の台帳・月で区切らない） {_usd(budget.get('relay_usd'))} / "
                f"${budget.get('global_cap_usd')}（残り {_usd(budget.get('global_left_usd'))}）"
            )
    lines += ["", "## 6. 限界", ""]
    lines += [f"- {x}" for x in LIMITS]
    versions = {k: v for d in docs for k, v in (d.get("versions") or {}).items()}
    if versions:
        lines += ["", "版:", ""]
        for label, v in sorted(versions.items()):
            lines.append(
                f"- {label}: commit {str(v.get('commit'))[:12]}・バイナリ {str(v.get('binary_sha256'))[:12]}・"
                f"設定 {str(v.get('config_sha'))[:12]}・取り込み {str(v.get('capture_sha'))[:12]}"
            )
    return "\n".join(lines) + "\n"


def write_summary(
    docs: Sequence[Mapping],
    *,
    path: Path | None = None,
    budget: Mapping | None = None,
    denylist: Path | None = None,
    iters: int = 10_000,
    errors: Sequence[Mapping] = (),
) -> Path:
    words, _ = load_denylist(denylist)
    text = summary_md(docs, budget=budget, iters=iters, errors=errors)
    _require_publishable(text, words)
    path = Path(path or results_dir() / "summary.md")
    _atomic_write(path, text)
    return path
