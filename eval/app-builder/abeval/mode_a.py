"""Mode A（ツール単体・単発の生成・GAP-30 §4.6）。

1 回の生成で出た IR を採点する（自己修正の往復は Mode B だけで測る。製品の誤りの整形を Python に写さない）。

1. 課題ごとに fixture を作る（課題の間で名前が重なるので、課題ごとに作って片付ける）。
2. 依頼文に fixture の「参考情報」（テーブルの表示名・id・列の key と型、フォルダー・ファイルの id）を添える。
3. 生成: model `claude-haiku-5-5`・取り込んだ system と max_tokens・tools は取り込んだ emit_workflow だけ
   （defer_loading を外す）・thinking adaptive・`output_config.effort`・tool_choice auto・stream。temperature と
   prefill は送らない（abeval.anthropic_client が止める）。送り先は abeval.proxy で、会話 ID のヘッダーに試行の
   id を載せる（abeval.proxy が試行に費用を割り当てる）。並列度は `[mode_a] parallel`。
4. 結果を分ける: refusal（停止理由）→ truncated（max_tokens）→ emit（emit_workflow の呼び出しがある）→
   asked（聞き返し）→ no_emit。
5. S1 はその版の `/workflows/validate`、S2s・S2w・S5 は ir_lint と課題の検査、S2d・S3 は stub の配管
   （名前 `abeval-<key8>-<役割>`・LLM のノードは stub のモデル・`stub_ok` の run_case・終わったら消す）。
6. 生成はキャッシュ（`data/cache/trials/<key>.json`）に置き、同じ条件なら生成しない（台帳の行が増えない）。
   キーの依頼文は fixture の id を印にした文（実験のたびに id が変わってもキーが同じ）。記録の IR も印に戻す。
7. V1 の採点し直し（`generate=False`）: 生成はせず、キャッシュの生成をその版で採点し直す（取り込みの sha が
   同じことを呼び出し側が確かめる）。

会話の全文（応答の文・思考）はキャッシュにだけ置き、結果には入れない。
"""

from __future__ import annotations

import copy
import datetime as dt
from collections.abc import Callable, Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

from abeval import fixtures
from abeval.anthropic_client import AnthropicClient, MessageResult
from abeval.cache import TrialCache, trial_key
from abeval.capture import Capture
from abeval.config import Config
from abeval.fixtures import FixtureError, FixtureRecord
from abeval.grade.static import LintCatalog
from abeval.grading import (
    Lint,
    headline,
    s1_of,
    safe_cleanup,
    static_roles,
    stub_plumbing,
    validate_roles,
)
from abeval.proxy_control import ProxyControl
from abeval.shiki import ShikiClient, ShikiError
from abeval.tasks import Task
from abeval.trial import (
    EMIT_TOOL,
    MODEL,
    TrialSpec,
    ask_terms,
    assign_roles,
    canonical,
    emitted_irs,
    is_question,
    placeholder_ids,
    prompt_text,
)

STATUSES = ("emit", "no_emit", "asked", "truncated", "refusal")


class RunStopped(RuntimeError):
    """これ以上送っても費用の上限などで拒まれる（実行を打ち切る）。"""


@dataclass
class ModeAContext:
    cfg: Config
    client: ShikiClient
    anthropic: AnthropicClient | None
    capture: Capture
    lint: Lint
    catalog: LintCatalog
    cache: TrialCache
    binary_sha: str
    config_sha: str
    ctrl: ProxyControl | None = None
    run_id: str | None = None
    generate: bool = True
    stub_plumbing: bool = True
    now: Callable[[], dt.datetime] = fixtures.now_utc


@dataclass
class Outcome:
    trials: list[dict] = field(default_factory=list)
    errors: list[dict] = field(default_factory=list)
    stopped: str | None = None


def build_request(capture: Capture, prompt: str, effort: str | None) -> dict:
    """Mode A の要求（temperature・top_p・top_k・prefill は無い・tool_choice は auto）。"""
    body: dict = {
        "model": MODEL,
        "max_tokens": capture.max_tokens,
        "system": capture.system,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [capture.emit_tool()],
        "tool_choice": {"type": "auto"},
        "stream": True,
    }
    if effort is not None:
        body["thinking"] = {"type": "adaptive"}
        body["output_config"] = {"effort": effort}
    return body


def classify(res: MessageResult, terms: tuple[str, ...] = ()) -> str:
    """生成の結果を 5 つに分ける。"""
    if res.stop_reason == "refusal":
        return "refusal"
    if res.stop_reason == "max_tokens":
        return "truncated"
    if emitted_irs(res.tool_uses(EMIT_TOOL)):
        return "emit"
    if is_question(res.text(), terms):
        return "asked"
    return "no_emit"


def _stop_category(res: MessageResult) -> str | None:
    d = res.stop_details
    return str(d.get("category")) if isinstance(d, Mapping) and d.get("category") else None


def _is_budget_stop(res: MessageResult) -> bool:
    err = res.error or {}
    msg = str(err.get("message") or "")
    return (res.status == 400 and msg.startswith("abeval:")) or res.status == 403


def key_for(ctx: ModeAContext, task: Task, ts: TrialSpec) -> str:
    return trial_key(
        task_id=task.id,
        mode="a",
        phrasing=ts.phrasing,
        prompt=prompt_text(task, ts.phrasing, placeholder_ids(task), with_reference=True),
        fixtures=task.fixtures,
        trial=ts.n,
        condition=ts.condition(),
        config_sha=ctx.config_sha,
        capture_sha=ctx.capture.sha,
        binary_sha=ctx.binary_sha,
    )


def generate(ctx: ModeAContext, task: Task, rec: FixtureRecord, ts: TrialSpec) -> dict:
    """1 試行の生成（キャッシュにあれば使う）。{"key", "gen" | "error", "from_cache"}。"""
    key = key_for(ctx, task, ts)
    cached = ctx.cache.get(key)
    if cached and isinstance(cached.get("gen"), dict):
        return {"key": key, "gen": cached["gen"], "from_cache": True}
    if not ctx.generate or ctx.anthropic is None:
        return {
            "key": key,
            "error": {"kind": "no_cached_generation", "detail": "生成がキャッシュに無い"},
        }
    prompt = prompt_text(task, ts.phrasing, rec.refs, with_reference=True)
    body = build_request(ctx.capture, prompt, ts.effort)
    session = ts.trial_id
    if ctx.ctrl is not None and ctx.run_id is not None:
        ctx.ctrl.register_session(session, ts.trial_id, ctx.run_id)
    res = ctx.anthropic.create(body, session=session)
    attempts = [a.to_dict() for a in res.attempts]
    if not (res.status == 200 and res.complete and res.error is None):
        kind = "budget" if _is_budget_stop(res) else "generation_failed"
        return {
            "key": key,
            "error": {
                "kind": kind,
                "status": res.status,
                "type": (res.error or {}).get("type"),
                "detail": str((res.error or {}).get("message") or "")[:300],
                "attempts": attempts,
            },
        }
    status = classify(res, ask_terms(task))
    irs = [canonical(ir, rec.refs, task) for ir in emitted_irs(res.tool_uses(EMIT_TOOL))]
    gen = {
        "status": status,
        "stop_reason": res.stop_reason,
        "refusal_category": _stop_category(res),
        "model": res.model,
        "usage": res.usage,
        "cost_micros": ctx.cfg.pricing.cost_micros(MODEL, res.usage) if res.usage else 0,
        "attempts": attempts,
        "secs": round(res.secs, 3),
        "emit_count": len(res.tool_uses(EMIT_TOOL)),
        "irs": irs,
    }
    # 会話の全文（応答のブロック）はキャッシュにだけ置く（.gitignore・結果には入れない）。
    ctx.cache.put(key, {"key": key, "trial": ts.to_dict(), "gen": gen, "response": res.content})
    return {"key": key, "gen": gen, "from_cache": False}


def grade(
    ctx: ModeAContext, task: Task, rec: FixtureRecord, ts: TrialSpec, key: str, gen: dict
) -> dict:
    """1 試行の採点（S1・S2s・S2w・S2d・S3・S5・S8）。"""
    out: dict = {
        **ts.to_dict(),
        "key": key,
        "status": gen["status"],
        "stop_reason": gen.get("stop_reason"),
        "refusal_category": gen.get("refusal_category"),
        "model": gen.get("model"),
        "usage": gen.get("usage") or {},
        "cost_micros": gen.get("cost_micros", 0),
        "retries": max(len(gen.get("attempts") or []) - 1, 0),
        "secs": gen.get("secs"),
        "emit_count": gen.get("emit_count", 0),
        "s8": {
            "refusal": gen["status"] == "refusal",
            "asked": gen["status"] == "asked",
            "no_emit": gen["status"] in ("no_emit", "asked"),
        },
    }
    roles, missing = assign_roles(task, gen.get("irs") or [], prefer="first")
    out["irs"] = copy.deepcopy(roles)
    out["missing_roles"] = missing
    if gen["status"] != "emit" or not roles:
        out.update({"s1": False, "s2s": None, "s2w": None, "s2d": None, "s3": None, "s5": None})
        out["headline"] = False
        return out
    validation = validate_roles(ctx.client, task, rec, roles)
    out["validation"] = validation
    out["s1"] = s1_of(validation, missing)
    st = static_roles(task, roles, ctx.lint, ctx.catalog, key[:12])
    out["static"] = {k: v for k, v in st.items() if k != "node_ports"}
    out["s2s"], out["s2w"], out["s5"] = st["s2s"], st["s2w"], st["s5"]
    out["s2d"] = out["s3"] = None
    if out["s1"] and ctx.stub_plumbing:
        stub = stub_plumbing(
            ctx.client,
            task,
            rec,
            roles,
            tag=key[:8],
            lint_ports=st["node_ports"],
            now=ctx.now(),
            timeout=ctx.cfg.mode_b.run_case_timeout_secs,
        )
        out["stub"] = stub
        out["s2d"], out["s3"] = stub.get("s2d"), stub.get("s3")
    out["headline"] = headline(out["s1"], out["s2s"], out["s2d"])
    return out


def run_task(ctx: ModeAContext, task: Task, specs: list[TrialSpec], out: Outcome) -> None:
    """1 課題の試行（fixture を作る → 並列に生成 → 順に採点 → 片付け）。"""
    try:
        rec = fixtures.create_fixtures(ctx.client, task.fixtures, now=ctx.now())
    except (FixtureError, fixtures.SentinelMissing) as e:
        for ts in specs:
            out.errors.append({**ts.to_dict(), "kind": "fixture", "detail": str(e)[:500]})
        if getattr(e, "record", None) is not None:
            safe_cleanup(ctx.client, e.record)
        return
    try:
        with ThreadPoolExecutor(max_workers=max(1, ctx.cfg.mode_a.parallel)) as pool:
            gens = list(pool.map(lambda ts: generate(ctx, task, rec, ts), specs))
        for ts, g in zip(specs, gens, strict=True):
            if "error" in g:
                out.errors.append({**ts.to_dict(), "key": g["key"], **g["error"]})
                if g["error"]["kind"] == "budget":
                    out.stopped = g["error"]["detail"]
                continue
            try:
                trial = grade(ctx, task, rec, ts, g["key"], g["gen"])
            except (ShikiError, TimeoutError) as e:
                out.errors.append(
                    {**ts.to_dict(), "key": g["key"], "kind": "grade", "detail": str(e)[:500]}
                )
                continue
            trial["from_cache"] = g["from_cache"]
            out.trials.append(trial)
    finally:
        problems = safe_cleanup(ctx.client, rec)
        if problems:
            out.errors.append(
                {"task": task.id, "kind": "cleanup", "detail": "; ".join(problems)[:500]}
            )


def run(ctx: ModeAContext, specs: list[TrialSpec], tasks: Mapping[str, Task]) -> Outcome:
    """試行の列を課題ごとに流す（費用の上限で拒まれたら残りを打ち切る）。"""
    out = Outcome()
    by_task: dict[str, list[TrialSpec]] = {}
    for ts in specs:
        by_task.setdefault(ts.task_id, []).append(ts)
    for task_id, group in by_task.items():
        if out.stopped:
            for ts in group:
                out.errors.append({**ts.to_dict(), "kind": "stopped", "detail": out.stopped})
            continue
        run_task(ctx, tasks[task_id], group, out)
    return out
