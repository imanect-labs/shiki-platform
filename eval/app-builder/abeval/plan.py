"""計画と見積もり（`python -m abeval plan`・`run` の前段・GAP-30 §6.2・§13）。

- 試行の列: Mode A は 課題 × 言い方 × 試行の番号 × effort。Mode B は 課題 × 試行の番号 × effort で、言い方は
  試行の番号で回す（§7.4・追加の費用が掛からない）。
- 見積もり: 1 試行の中央と最悪は `eval.toml` の `[estimates]`（推測。E0 の実測で置き換える）。Mode A は、
  取り込みがあれば abeval.proxy の予約と同じ式（入力 ≦ 本文のバイト数・出力 ≦ max_tokens）の額と比べて
  大きい方を最悪にする。
- 拒む: 最悪が 1 回の上限（`run_cap_usd`・$10）を超える（分け方を示す）・評価の台帳の累計＋最悪が
  `eval_cap_usd`（$35）を超える・手元の中継の台帳の累計＋最悪が `global_cap_usd`（$95）を超える。
- `--go` のときだけ、abeval.proxy に実行を登録する（上限は min(最悪 × 1.2, run_cap)）。登録の無い要求は
  abeval.proxy が拒むので、計画を通らない実行は費用を使えない。
"""

from __future__ import annotations

import datetime as dt
import json
import math
from collections.abc import Sequence
from dataclasses import dataclass, field

from abeval.config import Config, display_path
from abeval.ledger import Ledger, read_relay_tagged_usd, read_relay_total_usd
from abeval.pricing import micros_to_usd
from abeval.proxy_control import ProxyControl, safe_id
from abeval.tasks import PHRASINGS
from abeval.trial import MODEL, TrialSpec

MODES = ("a", "b")
EFFORTS = ("low", "medium", "high")
CAP_MARGIN = 1.2


class PlanError(ValueError):
    """計画の指定の誤り。"""


@dataclass(frozen=True)
class RunSpec:
    """1 回の実行（`run` の 1 回）の指定。"""

    exp: str
    mode: str
    label: str
    tasks: tuple[str, ...]
    phrasings: tuple[str, ...] = PHRASINGS
    trials: int = 3
    efforts: tuple[str | None, ...] = ("medium",)
    max_steps: int | None = None

    def __post_init__(self):
        if self.mode not in MODES:
            raise PlanError(f"mode は {MODES} のどれかです: {self.mode!r}")
        if not self.tasks:
            raise PlanError("課題がありません")
        if self.trials < 1:
            raise PlanError("trials は 1 以上です")
        bad = [p for p in self.phrasings if p not in PHRASINGS]
        if bad or not self.phrasings:
            raise PlanError(f"言い方は {PHRASINGS} から選びます: {bad}")
        bad = [e for e in self.efforts if e is not None and e not in EFFORTS]
        if bad or not self.efforts:
            raise PlanError(f"effort は {EFFORTS} か auto です: {bad}")
        safe_id(self.exp)

    def trial_specs(self) -> list[TrialSpec]:
        out = []
        for task in self.tasks:
            for effort in self.efforts:
                if self.mode == "a":
                    pairs = [(ph, n) for ph in self.phrasings for n in range(self.trials)]
                else:
                    pairs = [
                        (self.phrasings[n % len(self.phrasings)], n) for n in range(self.trials)
                    ]
                for ph, n in pairs:
                    out.append(
                        TrialSpec(
                            self.exp, self.mode, self.label, task, ph, n, effort, self.max_steps
                        )
                    )
        return out

    def to_dict(self) -> dict:
        return {
            "exp": self.exp,
            "mode": self.mode,
            "label": self.label,
            "tasks": list(self.tasks),
            "phrasings": list(self.phrasings),
            "trials": self.trials,
            "efforts": list(self.efforts),
            "max_steps": self.max_steps,
        }


@dataclass
class Plan:
    spec: RunSpec
    trials: list[TrialSpec]
    central_usd: float
    worst_usd: float
    eval_spent_usd: float
    relay_spent_usd: float | None
    run_id: str
    cap_usd: float
    caps: dict = field(default_factory=dict)
    problems: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    splits: list[list[str]] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.problems

    def to_dict(self) -> dict:
        return {
            "spec": self.spec.to_dict(),
            "trials": len(self.trials),
            "central_usd": round(self.central_usd, 4),
            "worst_usd": round(self.worst_usd, 4),
            "eval_spent_usd": round(self.eval_spent_usd, 6),
            "relay_spent_usd": None
            if self.relay_spent_usd is None
            else round(self.relay_spent_usd, 6),
            "run_id": self.run_id,
            "cap_usd": round(self.cap_usd, 4),
            "caps": self.caps,
            "problems": self.problems,
            "notes": self.notes,
            "splits": self.splits,
        }


def mode_a_reserve_usd(cfg: Config, body_bytes: int, max_tokens: int) -> float:
    """Mode A の 1 回の要求の最悪（abeval.proxy の予約と同じ式）。"""
    return micros_to_usd(cfg.pricing.reserve_micros(MODEL, body_bytes, max_tokens))


def trial_estimate(
    cfg: Config, ts: TrialSpec, reserve_usd: float | None = None
) -> tuple[float, float]:
    est = cfg.estimates[ts.mode]
    worst = est.worst_for(ts.effort, ts.max_steps)
    if ts.mode == "a" and reserve_usd is not None:
        worst = max(worst, reserve_usd)
    return est.central, worst


def spent(cfg: Config) -> tuple[float, float | None, list[str]]:
    """(評価の累計, 手元の中継の台帳の累計（無ければ None）, 注記)。どちらも読むだけ。

    評価の累計は、評価の台帳と、手元の中継の台帳の評価の分（tag が `abeval/` で始まる行）の大きい方（評価の台帳が
    消えても少なく数えない）。注記のパスはホームを `~` にする（結果の runs[] に入るので利用者名を残さない）。
    """
    notes = []
    eval_usd = micros_to_usd(Ledger(cfg.paths.ledger).refresh().total_micros)
    try:
        relay = read_relay_total_usd(cfg.paths.relay_ledger)
        tagged = read_relay_tagged_usd(cfg.paths.relay_ledger, cfg.proxy.tag_prefix)
        if tagged > eval_usd:
            notes.append(
                f"手元の中継の台帳の評価の分 ${tagged:.4f} が評価の台帳 ${eval_usd:.4f} より大きいので、そちらで数える"
            )
            eval_usd = tagged
    except FileNotFoundError:
        relay = None
        notes.append(
            f"手元の中継の台帳がありません（{display_path(cfg.paths.relay_ledger)}）。外側の残りは手元の中継が強制する"
        )
    return eval_usd, relay, notes


def _split_by_tasks(
    cfg: Config, spec: RunSpec, worst_by_task: dict[str, float], cap: float
) -> list[list[str]]:
    """課題を順に詰めて、どの組も最悪が上限以内になる分け方。"""
    groups: list[list[str]] = [[]]
    total = 0.0
    for task in spec.tasks:
        w = worst_by_task[task]
        if groups[-1] and total + w > cap:
            groups.append([])
            total = 0.0
        groups[-1].append(task)
        total += w
    return groups


def make_plan(
    spec: RunSpec,
    cfg: Config,
    *,
    eval_spent_usd: float,
    relay_spent_usd: float | None,
    reserve_usd: float | None = None,
    now: dt.datetime | None = None,
    notes: Sequence[str] = (),
) -> Plan:
    now = now or dt.datetime.now(dt.UTC)
    trials = spec.trial_specs()
    central = worst = 0.0
    worst_by_task: dict[str, float] = {}
    for ts in trials:
        c, w = trial_estimate(cfg, ts, reserve_usd)
        central += c
        worst += w
        worst_by_task[ts.task_id] = worst_by_task.get(ts.task_id, 0.0) + w
    caps = cfg.caps
    run_id = safe_id(f"{spec.exp}.{spec.label}.{spec.mode}.{now.strftime('%Y%m%dT%H%M%S')}")
    plan = Plan(
        spec=spec,
        trials=trials,
        central_usd=central,
        worst_usd=worst,
        eval_spent_usd=eval_spent_usd,
        relay_spent_usd=relay_spent_usd,
        run_id=run_id,
        cap_usd=min(max(worst, 0.01) * CAP_MARGIN, caps.run_cap_usd),
        caps={"run": caps.run_cap_usd, "eval": caps.eval_cap_usd, "global": caps.global_cap_usd},
        notes=list(notes),
    )
    if worst > caps.run_cap_usd:
        groups = _split_by_tasks(cfg, spec, worst_by_task, caps.run_cap_usd)
        plan.splits = groups
        plan.problems.append(
            f"最悪 ${worst:.2f} が 1 回の上限 ${caps.run_cap_usd:.2f} を超えます。"
            f"{max(len(groups), math.ceil(worst / caps.run_cap_usd))} 回以上に分けます"
        )
        too_big = [t for t, w in worst_by_task.items() if w > caps.run_cap_usd]
        if too_big:
            plan.problems.append(
                f"1 課題だけで上限を超えます（試行の数か effort を分ける）: {too_big}"
            )
    if eval_spent_usd + worst > caps.eval_cap_usd:
        plan.problems.append(
            f"評価の累計 ${eval_spent_usd:.4f}＋最悪 ${worst:.2f} が評価の上限 ${caps.eval_cap_usd:.2f} を超えます"
        )
    if relay_spent_usd is not None and relay_spent_usd + worst > caps.global_cap_usd:
        plan.problems.append(
            f"手元の中継の累計 ${relay_spent_usd:.4f}＋最悪 ${worst:.2f} が総額の上限 "
            f"${caps.global_cap_usd:.2f} を超えます"
        )
    return plan


def format_plan(plan: Plan) -> str:
    s = plan.spec
    lines = [
        f"計画: exp={s.exp} mode={s.mode} label={s.label}  課題 {len(s.tasks)}・言い方 {len(s.phrasings)}・"
        f"試行 {s.trials}・effort {[e or 'auto' for e in s.efforts]}"
        + (f"・手数 {s.max_steps}" if s.max_steps else "")
        + f" → {len(plan.trials)} 試行",
        f"見積もり（推測）: 中央 ${plan.central_usd:.4f}・最悪 ${plan.worst_usd:.4f}"
        f"（1 回の上限 ${plan.caps['run']:.2f}）",
        f"評価の台帳の累計 ${plan.eval_spent_usd:.4f}（上限 ${plan.caps['eval']:.2f}）",
    ]
    relay = plan.relay_spent_usd
    lines.append(
        "手元の中継の台帳の累計 "
        + ("（台帳が無い）" if relay is None else f"${relay:.4f}")
        + f"（上限 ${plan.caps['global']:.2f}）"
    )
    lines.append(f"abeval.proxy に登録する実行: {plan.run_id}（上限 ${plan.cap_usd:.4f}）")
    for n in plan.notes:
        lines.append(f"[注] {n}")
    for p in plan.problems:
        lines.append(f"[拒否] {p}")
    for i, g in enumerate(plan.splits, 1):
        lines.append(f"  分け方 {i}: --tasks {','.join(g)}")
    lines.append("=> " + ("実行できます" if plan.ok else "この計画では実行しません"))
    return "\n".join(lines)


def register(plan: Plan, ctrl: ProxyControl) -> dict:
    """abeval.proxy に実行を登録する（`--go` のときだけ呼ぶ）。"""
    if not plan.ok:
        raise PlanError("拒んだ計画は登録しません: " + "; ".join(plan.problems))
    return ctrl.register_run(plan.run_id, safe_id(plan.spec.exp), round(plan.cap_usd, 6))


def plan_json(plan: Plan) -> str:
    return json.dumps(plan.to_dict(), ensure_ascii=False, indent=1)
