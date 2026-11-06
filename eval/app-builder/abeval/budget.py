"""費用の残り（`python -m abeval budget`・GAP-30 §6.2）。

- 評価の台帳（`results/spend.jsonl`・abeval.proxy が書く・上限の正本）の累計と、実験ごと・実行（run）ごとの額。
- 手元の中継の台帳（`~/.local/state/shiki-campaign/anthropic-spend.jsonl`）の累計（外側の歯止め・評価の分も
  含む）。どちらも読むだけで、8787（手元の中継）へは繋がない。
- 残り = 上限 − 累計（評価は `eval_cap_usd`、総額は `global_cap_usd`）。
"""

from __future__ import annotations

import argparse
import json
from dataclasses import dataclass, field

from abeval.config import Config, load_config
from abeval.ledger import Ledger, LedgerCorrupt, read_relay_total_usd
from abeval.pricing import micros_to_usd


@dataclass
class RunSpend:
    exp: str | None
    micros: int = 0
    lines: int = 0
    usage_missing: int = 0


@dataclass
class Budget:
    eval_micros: int
    eval_cap_usd: float
    lines: int
    by_exp: dict[str, int] = field(default_factory=dict)
    by_run: dict[str, RunSpend] = field(default_factory=dict)
    relay_usd: float | None = None
    global_cap_usd: float = 95.0
    notes: list[str] = field(default_factory=list)

    @property
    def eval_usd(self) -> float:
        return micros_to_usd(self.eval_micros)

    @property
    def eval_left_usd(self) -> float:
        return self.eval_cap_usd - self.eval_usd

    @property
    def global_left_usd(self) -> float | None:
        return None if self.relay_usd is None else self.global_cap_usd - self.relay_usd

    def to_dict(self) -> dict:
        return {
            "eval_usd": round(self.eval_usd, 6),
            "eval_cap_usd": self.eval_cap_usd,
            "eval_left_usd": round(self.eval_left_usd, 6),
            "lines": self.lines,
            "by_exp_usd": {k: round(micros_to_usd(v), 6) for k, v in sorted(self.by_exp.items())},
            "by_run": {
                k: {
                    "exp": v.exp,
                    "usd": round(micros_to_usd(v.micros), 6),
                    "lines": v.lines,
                    "usage_missing": v.usage_missing,
                }
                for k, v in sorted(self.by_run.items())
            },
            "relay_usd": None if self.relay_usd is None else round(self.relay_usd, 6),
            "global_cap_usd": self.global_cap_usd,
            "global_left_usd": None
            if self.global_left_usd is None
            else round(self.global_left_usd, 6),
            "notes": self.notes,
        }


def summarize(cfg: Config) -> Budget:
    """台帳を読んで累計と残りを出す（壊れた評価の台帳は LedgerCorrupt で止める）。"""
    entries = Ledger(cfg.paths.ledger).entries()
    b = Budget(
        eval_micros=sum(e["cost_micros"] for e in entries),
        eval_cap_usd=cfg.caps.eval_cap_usd,
        lines=len(entries),
        global_cap_usd=cfg.caps.global_cap_usd,
    )
    for e in entries:
        exp = e.get("exp") if isinstance(e.get("exp"), str) else "-"
        b.by_exp[exp] = b.by_exp.get(exp, 0) + e["cost_micros"]
        run = e.get("run_id") if isinstance(e.get("run_id"), str) else "-"
        r = b.by_run.setdefault(run, RunSpend(exp))
        r.micros += e["cost_micros"]
        r.lines += 1
        r.usage_missing += 1 if e.get("usage_missing") else 0
    try:
        b.relay_usd = read_relay_total_usd(cfg.paths.relay_ledger)
    except FileNotFoundError:
        b.notes.append(f"手元の中継の台帳がありません: {cfg.paths.relay_ledger}")
    except LedgerCorrupt as e:
        b.notes.append(f"手元の中継の台帳が読めません: {e}")
    return b


def trial_spend(cfg: Config, trial_id: str, run_id: str | None = None) -> dict:
    """1 試行の費用（評価の台帳の行を試行で集める・会話の分は会話 ID のある行、run_case の分は active の行）。

    `statuses` は 200 以外の応答の状態コードの数（上流の 429・5xx、手元の中継の 403、送った後の誤りの 0）。
    """
    out: dict = {"chat_micros": 0, "run_case_micros": 0, "lines": 0, "usage_missing": 0}
    statuses: dict[str, dict[str, int]] = {"chat": {}, "run_case": {}}
    for e in Ledger(cfg.paths.ledger).entries():
        if e.get("trial") != trial_id or (run_id and e.get("run_id") != run_id):
            continue
        phase = "chat" if e.get("session") else "run_case"
        out[f"{phase}_micros"] += e["cost_micros"]
        out["lines"] += 1
        out["usage_missing"] += 1 if e.get("usage_missing") else 0
        if "status" in e and e["status"] != 200 and not e.get("captured"):
            k = str(e.get("status"))
            statuses[phase][k] = statuses[phase].get(k, 0) + 1
    out["total_micros"] = out["chat_micros"] + out["run_case_micros"]
    out["statuses"] = {k: dict(sorted(v.items())) for k, v in statuses.items()}
    return out


def format_budget(b: Budget) -> str:
    lines = [
        f"評価の台帳: 累計 ${b.eval_usd:.4f} / ${b.eval_cap_usd:.2f}（残り ${b.eval_left_usd:.4f}・"
        f"{b.lines} 行）",
    ]
    for exp, micros in sorted(b.by_exp.items()):
        lines.append(f"  実験 {exp}: ${micros_to_usd(micros):.4f}")
    for run, r in sorted(b.by_run.items()):
        miss = f"・usage が無く予約で数えた {r.usage_missing} 行" if r.usage_missing else ""
        lines.append(f"  実行 {run}（{r.exp}）: ${micros_to_usd(r.micros):.4f}・{r.lines} 行{miss}")
    if b.relay_usd is None:
        lines.append("手元の中継の台帳: （読めない）")
    else:
        lines.append(
            f"手元の中継の台帳: 累計 ${b.relay_usd:.4f} / ${b.global_cap_usd:.2f}"
            f"（残り ${b.global_left_usd:.4f}・評価の分を含む）"
        )
    lines += [f"[注] {n}" for n in b.notes]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m abeval budget", description="費用の累計と残り")
    ap.add_argument("--json", action="store_true", help="JSON で出す")
    args = ap.parse_args(argv)
    b = summarize(load_config())
    print(json.dumps(b.to_dict(), ensure_ascii=False, indent=1) if args.json else format_budget(b))
    return 0
