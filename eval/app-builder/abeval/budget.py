"""費用の残り（`python -m abeval budget`・GAP-30 §6.2）。

- 評価の台帳（`~/.local/share/shiki-eval/ledger.jsonl`・abeval.proxy が書く・上限の正本）の累計と、実験ごと・
  実行（run）ごとの額。
- 手元の中継の台帳（`~/.local/state/shiki-campaign/anthropic-spend.jsonl`）の累計（外側の歯止め・評価の分も
  含む）。どちらも読むだけで、8787（手元の中継）へは繋がない。
- 残り = 上限 − 累計（評価は `eval_cap_usd`、総額は `global_cap_usd`）。
- `ledger_guard`: 評価の台帳が消えた・別の台帳になったのを見つける（abeval.proxy は起動を拒み、doctor は NG）。
  (1) 前の既定の置き場（作業ツリーの `results/spend.jsonl`）に行が残っている。(2) 手元の中継の台帳のうち tag が
  `abeval/` で始まる行の累計が、評価の台帳の累計より大きい（作業ツリーを作り直す・`git clean -fdx`・別の checkout
  から流すと、評価の台帳が $0 から数え直しになり $35 の歯止めがすり抜けるため）。
- `--reconcile`（BUG-250）: abeval.proxy が SIGKILL・OOM で落ちると、処理中だった要求は手元の中継で課金されたのに
  評価の台帳に載らず、(2) で起動を拒まれる。手元の中継の台帳の評価の分を tag（`abeval/<exp>/<run>/<trial>`）ごとに
  評価の台帳と比べ、手元の中継の方が多い tag の差を、印（`kind: reconciled`）の付いた行として評価の台帳に足す
  （少なく数えない側に寄せる・手元の中継の台帳は読むだけ）。何をどれだけ足すかを先に表示し、`--yes` が無ければ
  書かない。書くのは abeval.proxy が止まっているとき（`ss` で 18200 に待ち受けが無い）だけで、評価の台帳の
  排他の錠（flock）を取ったまま読み直して足す。
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from collections.abc import Callable
from dataclasses import dataclass, field

from abeval.config import ROOT, Config, display_path, load_config
from abeval.ledger import (
    Ledger,
    LedgerCorrupt,
    read_relay_by_tag,
    read_relay_tagged_usd,
    read_relay_total_usd,
)
from abeval.pricing import micros_to_usd

# 前の既定の評価の台帳（作業ツリーの中・ここに行が残っていたら移すまで動かさない）。
LEGACY_LEDGER = ROOT / "results" / "spend.jsonl"
# 手元の中継の台帳の評価の分が、評価の台帳より大きいとみなす差（丸めと単価の差を許す）。
RELAY_TOLERANCE_USD = 0.01
RELAY_TOLERANCE_RATIO = 0.01
# 差を足した行の印（`budget --reconcile`）。
RECONCILED = "reconciled"
RECONCILE_HINT = (
    "abeval.proxy が SIGKILL・OOM で落ちた（処理中の要求が評価の台帳に載らなかった）なら、"
    "uv run python -m abeval budget --reconcile で足す行を確かめ、--yes で評価の台帳に足してから起動し直す"
    "（README の「上限と台帳」）"
)


def ledger_guard(cfg: Config) -> str | None:
    """評価の台帳が消えた・別の台帳になった印があれば理由（無ければ None）。どちらの台帳も読むだけ。"""
    legacy = LEGACY_LEDGER
    if (
        legacy.resolve() != cfg.paths.ledger.resolve()
        and legacy.is_file()
        and legacy.stat().st_size
    ):
        return (
            f"前の置き場の評価の台帳に行が残っています: {display_path(legacy)}。"
            f"{display_path(cfg.paths.ledger)} の末尾に足してから消します（README の「上限と台帳」）"
        )
    try:
        relay = read_relay_tagged_usd(cfg.paths.relay_ledger, cfg.proxy.tag_prefix)
    except FileNotFoundError:
        return None
    except LedgerCorrupt as e:
        return f"手元の中継の台帳が読めません: {e}"
    ours = micros_to_usd(Ledger(cfg.paths.ledger).refresh().total_micros)
    if relay > ours + max(RELAY_TOLERANCE_USD, RELAY_TOLERANCE_RATIO * relay):
        return (
            f"手元の中継の台帳の評価の分（tag {cfg.proxy.tag_prefix}/・${relay:.4f}）が、評価の台帳の累計"
            f"（${ours:.4f}・{display_path(cfg.paths.ledger)}）より大きい。評価の台帳が消えたか、別の台帳を"
            f"使っています（このままでは評価の上限がすり抜ける）。{RECONCILE_HINT}"
        )
    return None


# --- 差を足す（BUG-250） ---


@dataclass(frozen=True)
class Shortfall:
    """手元の中継の台帳の方が多い tag 1 つ（足す額 = 手元の中継の額 − 評価の台帳の額）。"""

    tag: str
    relay_micros: int
    relay_lines: int
    eval_micros: int

    @property
    def add_micros(self) -> int:
        return self.relay_micros - self.eval_micros

    def entry(self, now: str) -> dict:
        _, exp, run_id, trial = ([*self.tag.split("/", 3), None, None, None])[:4]
        return {
            "ts": now,
            "kind": RECONCILED,
            "exp": exp,
            "run_id": run_id,
            "trial": None if trial in (None, "-") else trial,
            "session": None,
            "tag": self.tag,
            "path": "reconcile",
            "usage": {},
            "cost_micros": self.add_micros,
            "relay_micros": self.relay_micros,
            "relay_lines": self.relay_lines,
            "eval_micros_before": self.eval_micros,
            "usage_missing": False,
            "captured": False,
        }


def _entry_tag(e: dict, prefix: str) -> str | None:
    tag = e.get("tag")
    if isinstance(tag, str) and tag:
        return tag
    exp, run_id = e.get("exp"), e.get("run_id")
    if isinstance(exp, str) and isinstance(run_id, str):
        return f"{prefix}/{exp}/{run_id}/{e.get('trial') or '-'}"
    return None


def shortfalls(cfg: Config, entries: list[dict] | None = None) -> list[Shortfall]:
    """手元の中継の台帳の評価の分が、評価の台帳より多い tag（どちらの台帳も読むだけ）。"""
    try:
        relay = read_relay_by_tag(cfg.paths.relay_ledger, cfg.proxy.tag_prefix)
    except FileNotFoundError:
        return []
    ours: dict[str, int] = {}
    for e in Ledger(cfg.paths.ledger).entries() if entries is None else entries:
        tag = _entry_tag(e, cfg.proxy.tag_prefix)
        if tag is not None:
            ours[tag] = ours.get(tag, 0) + e["cost_micros"]
    out = []
    for tag, (usd, n) in sorted(relay.items()):
        # 少なく数えないよう切り上げる（浮動小数の誤差の分は丸めてから）。
        micros = math.ceil(round(usd * 1_000_000, 3))
        got = ours.get(tag, 0)
        if micros > got:
            out.append(Shortfall(tag, micros, n, got))
    return out


def format_shortfalls(items: list[Shortfall]) -> str:
    if not items:
        return "足す行はありません（手元の中継の台帳の評価の分は、どの tag も評価の台帳を超えていない）"
    lines = ["評価の台帳に足す行（手元の中継の台帳の方が多い tag・kind=reconciled）:"]
    for s in items:
        lines.append(
            f"  {s.tag}: 手元の中継 ${micros_to_usd(s.relay_micros):.6f}（{s.relay_lines} 行）・評価の台帳 "
            f"${micros_to_usd(s.eval_micros):.6f} → 足す ${micros_to_usd(s.add_micros):.6f}"
        )
    total = sum(s.add_micros for s in items)
    lines.append(f"合計 ${micros_to_usd(total):.6f}（{len(items)} 行）")
    return "\n".join(lines)


def proxy_stopped(cfg: Config, ss: Callable[[int], str | None] | None = None) -> str | None:
    """abeval.proxy が止まっていなければ理由（待ち受けを `ss` で見る・繋がない）。"""
    if ss is None:
        from abeval.server_guard import run_ss

        ss = run_ss
    out = ss(cfg.proxy.port)
    if out is None:
        return "ss がありません（abeval.proxy が止まっているか確かめられない）"
    if any(line.strip() for line in out.splitlines()):
        return f"abeval.proxy（:{cfg.proxy.port}）が動いています。処理中の要求と二重に数えないよう、止めてから足す"
    return None


def reconcile(cfg: Config, *, ss: Callable[[int], str | None] | None = None) -> list[Shortfall]:
    """差を評価の台帳に足す（錠を取ったまま読み直して決める）。足した行（abeval.proxy が動いていれば ValueError）。"""
    why = proxy_stopped(cfg, ss)
    if why:
        raise ValueError(why)
    done: list[Shortfall] = []

    def build(entries: list[dict]) -> list[dict]:
        done[:] = shortfalls(cfg, entries)
        now = dt.datetime.now(dt.UTC).isoformat(timespec="seconds")
        return [s.entry(now) for s in done]

    Ledger(cfg.paths.ledger).append_locked(build)
    return done


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
        b.notes.append(f"手元の中継の台帳がありません: {display_path(cfg.paths.relay_ledger)}")
    except LedgerCorrupt as e:
        b.notes.append(f"手元の中継の台帳が読めません: {e}")
    why = ledger_guard(cfg)
    if why:
        b.notes.append(why)
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
    ap.add_argument(
        "--reconcile",
        action="store_true",
        help="手元の中継の台帳の評価の分のうち、評価の台帳に無い差を表示する（abeval.proxy が落ちた後）",
    )
    ap.add_argument("--yes", action="store_true", help="--reconcile の差を評価の台帳に足す")
    args = ap.parse_args(argv)
    if args.yes and not args.reconcile:
        ap.error("--yes は --reconcile と一緒に使います")
    if args.reconcile:
        return _reconcile_main(load_config(), yes=args.yes)
    b = summarize(load_config())
    print(json.dumps(b.to_dict(), ensure_ascii=False, indent=1) if args.json else format_budget(b))
    return 0


def _reconcile_main(cfg: Config, *, yes: bool) -> int:
    try:
        items = shortfalls(cfg)
    except LedgerCorrupt as e:
        print(f"台帳が読めません: {e}")
        return 1
    print(format_shortfalls(items))
    if not items:
        return 0
    if not yes:
        print(f"（--yes を付けたときだけ {display_path(cfg.paths.ledger)} に書く）")
        return 0
    try:
        done = reconcile(cfg)
    except (ValueError, LedgerCorrupt) as e:
        print(f"書きませんでした: {e}")
        return 1
    total = sum(s.add_micros for s in done)
    print(
        f"評価の台帳に {len(done)} 行（${micros_to_usd(total):.6f}）を足しました: "
        f"{display_path(cfg.paths.ledger)}"
    )
    return 0
