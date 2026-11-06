"""Mode B（製品のチャットで作らせ、実行まで確かめる・GAP-30 §4.7）。並列度は 1。

1 試行の流れ:
1. fixture を作る（作った id は試行の記録に残す）。
2. `POST /threads` でスレッドを作り、abeval.proxy に thread_id（会話 ID）→ 試行を割り当てる。
3. `POST /threads/{id}/messages {text, model, effort}` を送る（stub では model `default` で effort を省く・
   Auto は model と effort を省く）。応答の run_id を控える。
4. 直後に SSE（`last_event_id=0`）を読み、`approval_requested` を承認の方針で決める（abeval.approvals・既定で
   却下）。SSE は承認の要求を拾うためにだけ使い、完了は `GET messages` の `active_run_id` を `poll_secs` ごとに
   読み直して待つ（SSE が空で終われば読み直しに切り替える）。壁時計の上限（`wall_clock_secs`・6 分）を
   超えたら `POST …/runs/{run_id}/cancel` で取り消し、timeout として errors に入れる（採点しない）。
5. run が終わって最後の文章が聞き返しなら、課題の `clarifications` を 1 回だけ送る。
6. `GET messages` で tool_call・tool_result・workflow_ref・最後の文章を回収する。emit_workflow の入力の IR は
   その版の `/workflows/validate` にかけ直して、検証エラーの回数とコードを数える（結果の文を読まない）。
7. 最後の workflow_ref の版を読み、workflow_set の課題はきっかけの種類で役割を割り当てる。
8. 静的に採点し、保存されたワークフローそのもので run_case を流す（評価の版は実モデル。費用は abeval.proxy の
   active で試行に割り当てる）。あわせて stub の配管も流す。
9. 費用を試行に割り当て（評価の台帳の行を試行で集める）、`/org-admin/ai/usage` の前後の差と突き合わせる。
10. 片付ける（記録した id と印付きのもの。試行の前に無かったものが残れば報告する）。

ミニアプリ（T12）の差し込み口として、採点の後に呼ぶ hook（`ModeBContext.hooks`）を残す。会話の全文は
キャッシュにだけ置く（結果に入れない・H7 (a)）。
"""

from __future__ import annotations

import datetime as dt
import time
from collections import Counter
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import Any
from zoneinfo import ZoneInfo

import httpx

from abeval import fixtures
from abeval.approvals import ApprovalLog, ApprovalPolicy, handle_request, unapproved_side_effects
from abeval.budget import trial_spend
from abeval.cache import TrialCache, trial_key
from abeval.config import Config
from abeval.fixtures import FixtureError, FixtureRecord
from abeval.grade.static import LintCatalog
from abeval.grading import (
    Lint,
    codes,
    run_saved,
    safe_cleanup,
    static_roles,
    stub_plumbing,
    validate_roles,
)
from abeval.proxy_control import ProxyControl
from abeval.run_cases import Saved
from abeval.shiki import ShikiClient, ShikiError
from abeval.tasks import Task
from abeval.trial import (
    EMIT_TOOL,
    MODEL,
    STUB_MODEL,
    TrialSpec,
    ask_terms,
    assign_roles,
    canonical,
    concrete,
    is_question,
)

JST = ZoneInfo("Asia/Tokyo")
DEFAULT_MAX_STEPS = 6
TERMINAL_EVENTS = frozenset({"done", "error"})
TERMINAL_STATUS = frozenset({"done", "failed", "cancelled"})
SAVED_CLAIM = ("保存しました", "作成しました", "保存済み")
CANCEL_WAIT_POLLS = 15


class ChatTimeout(RuntimeError):
    def __init__(self, run_id: str):
        super().__init__(f"run {run_id} が壁時計の上限までに終わりません")
        self.run_id = run_id


@dataclass
class ModeBContext:
    cfg: Config
    client: ShikiClient
    policy: ApprovalPolicy
    lint: Lint
    catalog: LintCatalog
    cache: TrialCache
    binary_sha: str
    config_sha: str
    capture_sha: str
    ctrl: ProxyControl | None = None
    run_id: str | None = None
    stub: bool = False
    stub_plumbing: bool = True
    usage_check: bool = True
    clock: Callable[[], float] = time.monotonic
    sleep: Callable[[float], None] = time.sleep
    now: Callable[[], dt.datetime] = fixtures.now_utc
    hooks: list[Callable[[dict], dict]] = field(default_factory=list)
    # 依頼文を fixture を作った後で組み直す（stub の確かめで、本物の id を入れた calltool の文にする）。
    prompt_fn: Callable[[Task, TrialSpec, FixtureRecord], str] | None = None


@dataclass
class ChatResult:
    runs: list[str]
    approvals: ApprovalLog
    clarified: bool
    sse_errors: list[str]
    page: dict


@dataclass
class Transcript:
    tool_calls: list[dict] = field(default_factory=list)
    results: dict[str, dict] = field(default_factory=dict)
    workflow_refs: list[dict] = field(default_factory=list)
    final_text: str = ""
    steps_per_message: list[int] = field(default_factory=list)


# --- 会話 ---


def message_args(ctx: ModeBContext, ts: TrialSpec) -> dict:
    """発話の model と effort（stub は default で effort なし・Auto は両方省く）。"""
    if ctx.stub:
        return {"model": STUB_MODEL}
    if ts.effort is None:
        return {}
    return {"model": MODEL, "effort": ts.effort}


def _remaining(ctx: ModeBContext, deadline: float) -> float:
    return deadline - ctx.clock()


def wait_run(
    ctx: ModeBContext, thread_id: str, run_id: str, log: ApprovalLog, deadline: float, errors: list
) -> dict:
    """run の終わりまで待つ（SSE で承認の要求を拾い、完了は読み直しで判定）。最後のメッセージ一覧を返す。"""
    last_id = 0
    while True:
        if _remaining(ctx, deadline) <= 0:
            raise ChatTimeout(run_id)
        try:
            for ev in ctx.client.stream(
                thread_id, last_id, timeout=max(_remaining(ctx, deadline), 1.0), keepalive=True
            ):
                if isinstance(ev.get("id"), int):
                    last_id = ev["id"]
                data = ev.get("data") or {}
                kind = data.get("type")
                if kind == "approval_requested":
                    handle_request(ctx.client, thread_id, run_id, data, ctx.policy, log)
                elif kind in TERMINAL_EVENTS or (
                    kind == "status" and data.get("status") in TERMINAL_STATUS
                ):
                    break
                if _remaining(ctx, deadline) <= 0:
                    break
        except (httpx.HTTPError, ShikiError) as e:
            errors.append(f"sse: {type(e).__name__}")
        page = ctx.client.messages(thread_id)
        if not page.get("active_run_id") and not page.get("queued_runs"):
            return page
        if _remaining(ctx, deadline) <= 0:
            raise ChatTimeout(run_id)
        ctx.sleep(ctx.cfg.mode_b.poll_secs)


def cancel_and_wait(ctx: ModeBContext, thread_id: str, run_id: str) -> bool:
    """run を取り消し、終わるまで少し待つ（終わったら True）。"""
    try:
        page = ctx.client.messages(thread_id)
        target = page.get("active_run_id") or run_id
        ctx.client.cancel_chat_run(thread_id, target)
    except ShikiError:
        pass
    for _ in range(CANCEL_WAIT_POLLS):
        try:
            page = ctx.client.messages(thread_id)
        except ShikiError:
            return False
        if not page.get("active_run_id") and not page.get("queued_runs"):
            return True
        ctx.sleep(ctx.cfg.mode_b.poll_secs)
    return False


def drive_chat(
    ctx: ModeBContext, task: Task, ts: TrialSpec, thread_id: str, prompt: str
) -> ChatResult:
    """依頼を送り、承認を扱い、聞き返しに 1 回だけ答える（壁時計の上限は試行の会話の全体）。"""
    deadline = ctx.clock() + ctx.cfg.mode_b.wall_clock_secs
    log = ApprovalLog()
    sse_errors: list[str] = []
    args = message_args(ctx, ts)
    run_id = ctx.client.post_message(thread_id, prompt, **args)["run_id"]
    runs = [run_id]
    page = wait_run(ctx, thread_id, run_id, log, deadline, sse_errors)
    clarified = False
    if is_question(parse_transcript(page).final_text, ask_terms(task)):
        run_id = ctx.client.post_message(thread_id, task.clarifications, **args)["run_id"]
        runs.append(run_id)
        clarified = True
        page = wait_run(ctx, thread_id, run_id, log, deadline, sse_errors)
    return ChatResult(runs, log, clarified, sse_errors, page)


# --- 記録の読み取り ---


def parse_transcript(page: Mapping) -> Transcript:
    """メッセージ一覧から tool_call・tool_result・workflow_ref・最後の文章を読む（知らないブロックは無視）。"""
    msgs = [m for m in page.get("messages") or [] if isinstance(m, Mapping)]
    msgs = sorted(enumerate(msgs), key=lambda p: (str(p[1].get("created_at") or ""), p[0]))
    t = Transcript()
    for _, m in msgs:
        steps = []
        texts = []
        for b in m.get("content") or []:
            if not isinstance(b, Mapping):
                continue
            kind = b.get("type")
            if kind == "tool_call":
                t.tool_calls.append(
                    {
                        "id": b.get("id"),
                        "name": b.get("name"),
                        "input": b.get("input"),
                        "step": b.get("step"),
                    }
                )
                if isinstance(b.get("step"), int):
                    steps.append(b["step"])
            elif kind == "tool_result":
                t.results[str(b.get("tool_call_id"))] = {
                    "ok": b.get("ok", True),
                    "content": b.get("content") or "",
                }
            elif kind == "workflow_ref" and isinstance(b.get("workflow"), Mapping):
                w = b["workflow"]
                t.workflow_refs.append(
                    {"id": w.get("id"), "name": w.get("name"), "version": w.get("version")}
                )
            elif kind == "text" and m.get("role") == "assistant":
                texts.append(str(b.get("text") or ""))
        if m.get("role") == "assistant":
            t.steps_per_message.append(max(steps) + 1 if steps else 0)
            if texts:
                t.final_text = "".join(texts)
    return t


def latest_refs(refs: list[dict]) -> list[dict]:
    """ワークフローごとの最後の参照（最後に出た順）。"""
    last: dict[str, dict] = {}
    for r in refs:
        if r.get("id"):
            last.pop(str(r["id"]), None)
            last[str(r["id"])] = r
    return list(last.values())


def round_trips(ctx: ModeBContext, t: Transcript, max_steps: int) -> dict:
    """S6（emit の回数・検証エラーの回数とコード・手数・上限・tool_search で emit_workflow を読めたか）。"""
    emits = [c for c in t.tool_calls if c.get("name") == EMIT_TOOL]
    error_codes: Counter = Counter()
    errors = 0
    for c in emits:
        ir = c.get("input", {}).get("ir") if isinstance(c.get("input"), Mapping) else None
        if not isinstance(ir, Mapping):
            error_codes["abeval.no_ir"] += 1
            errors += 1
            continue
        v = ctx.client.validate(ir)
        cs = codes(v.get("errors"))
        errors += len(cs)
        error_codes.update(cs)
    searched = [c for c in t.tool_calls if c.get("name") == "tool_search"]
    loaded = bool(emits) or any(
        EMIT_TOOL in str(t.results.get(str(c.get("id")), {}).get("content", "")) for c in searched
    )
    steps = max(t.steps_per_message, default=0)
    return {
        "emit_count": len(emits),
        "emit_failed": sum(
            1 for c in emits if not t.results.get(str(c.get("id")), {}).get("ok", True)
        ),
        "validation_errors": errors,
        "validation_codes": dict(sorted(error_codes.items())),
        "steps": steps,
        "hit_step_limit": steps >= max_steps,
        "tool_search_used": bool(searched),
        "emit_loaded": loaded,
        "tool_calls": dict(sorted(Counter(str(c.get("name")) for c in t.tool_calls).items())),
    }


# --- 片付けと費用 ---


def snapshot(client: ShikiClient) -> dict[str, set[str]]:
    return {
        "workflows": {w["id"] for w in client.workflows()},
        "tables": {t["id"] for t in client.tables()},
        "threads": {t["id"] for t in client.threads()},
    }


def _usage_micros(client: ShikiClient, day_from: dt.date, day_to: dt.date) -> int | None:
    try:
        u = client.usage(from_=day_from.isoformat(), to=day_to.isoformat())
    except ShikiError:
        return None
    v = (u.get("totals") or {}).get("cost_usd_micros")
    return int(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def reconcile(ledger_micros: int, usage_delta: int | None, tolerance: float = 0.01) -> dict:
    """評価の台帳と製品の会計（usage の前後の差）の突き合わせ（1% 以内・1 マイクロ USD の丸めは許す）。"""
    if usage_delta is None:
        return {"ledger_micros": ledger_micros, "usage_micros": None, "ok": None}
    diff = abs(ledger_micros - usage_delta)
    ok = diff <= max(tolerance * max(ledger_micros, usage_delta), 1)
    return {
        "ledger_micros": ledger_micros,
        "usage_micros": usage_delta,
        "diff_micros": diff,
        "ok": ok,
    }


# --- 1 試行 ---


def key_for(ctx: ModeBContext, task: Task, ts: TrialSpec, prompt: str) -> str:
    cond = {**ts.condition(), "stub": ctx.stub}
    return trial_key(
        task_id=task.id,
        mode="b",
        phrasing=ts.phrasing,
        prompt=prompt,
        fixtures=task.fixtures,
        trial=ts.n,
        condition=cond,
        config_sha=ctx.config_sha,
        capture_sha=ctx.capture_sha,
        binary_sha=ctx.binary_sha,
    )


def _saved(
    ctx: ModeBContext, task: Task, rec: FixtureRecord, refs: list[dict]
) -> tuple[dict, list]:
    """最後の参照の版を読む。(役割 → {id, version, ir（印に戻したもの）}, 割り当たらない役割)。"""
    loaded = []
    for r in refs:
        doc = ctx.client.get_workflow_version(str(r["id"]), int(r["version"]))
        rec.add("workflow", str(r["id"]), str(r.get("name") or ""))
        loaded.append({"id": str(r["id"]), "version": int(r["version"]), "ir": doc.get("ir") or {}})
    roles, missing = assign_roles(task, [x["ir"] for x in loaded], prefer="last")
    out = {}
    for role, ir in roles.items():
        hit = next(x for x in reversed(loaded) if x["ir"] == ir)
        out[role] = {
            "id": hit["id"],
            "version": hit["version"],
            "ir": canonical(ir, rec.refs, task),
        }
    return out, missing


def grade_saved(
    ctx: ModeBContext, task: Task, rec: FixtureRecord, saved: dict, key: str, ts: TrialSpec
) -> dict:
    """保存されたワークフローの採点（静的・実行・stub の配管）。"""
    role_irs = {r: s["ir"] for r, s in saved.items()}
    st = static_roles(task, role_irs, ctx.lint, ctx.catalog, key[:12])
    validation = validate_roles(ctx.client, task, rec, role_irs)
    out: dict[str, Any] = {
        "static": {k: v for k, v in st.items() if k != "node_ports"},
        "s2s": st["s2s"],
        "s2w": st["s2w"],
        "s5": st["s5"],
        "revalidation": validation,
    }
    wfs = {}
    for role, s in saved.items():
        ports = validation[role].get("node_ports") or st["node_ports"].get(role) or {}
        wfs[role] = Saved(s["id"], s["version"], ports, concrete(s["ir"], rec.refs, task), {})
    if ctx.ctrl is not None and ctx.run_id is not None:
        ctx.ctrl.set_active(ctx.run_id, ts.trial_id)
    try:
        runs = run_saved(
            ctx.client,
            task,
            rec,
            wfs,
            stub=ctx.stub,
            now=ctx.now(),
            timeout=ctx.cfg.mode_b.run_case_timeout_secs,
        )
    finally:
        if ctx.ctrl is not None and ctx.run_id is not None:
            ctx.ctrl.set_active(None)
    out["runs"] = runs
    out["s3"], out["s4"] = runs.get("s3"), runs.get("s4")
    s2d = runs.get("s2d")
    if ctx.stub_plumbing:
        stub = stub_plumbing(
            ctx.client,
            task,
            rec,
            role_irs,
            tag=key[:8],
            lint_ports=st["node_ports"],
            now=ctx.now(),
            timeout=ctx.cfg.mode_b.run_case_timeout_secs,
        )
        out["stub"] = stub
        if stub.get("s2d") is not None:
            s2d = bool(s2d) or stub["s2d"]
    out["s2d"] = s2d
    return out


def run_trial(ctx: ModeBContext, task: Task, ts: TrialSpec) -> tuple[dict | None, list[dict]]:
    """1 試行（結果は採点した記録、基盤の失敗なら None）。(記録, errors)。"""
    prompt = task.phrasings[ts.phrasing]
    key = key_for(ctx, task, ts, prompt)
    cached = ctx.cache.get(key)
    if cached and isinstance(cached.get("result"), dict):
        return {**cached["result"], "from_cache": True}, []
    errors: list[dict] = []
    base = {**ts.to_dict(), "key": key, "mode": "b"}
    before = snapshot(ctx.client)
    rec = fixtures.new_record(ctx.client.user)
    started = ctx.clock()
    day0 = ctx.now().astimezone(JST).date()
    usage0 = _usage_micros(ctx.client, day0, day0) if ctx.usage_check else None
    trial: dict | None = None
    chat = None
    try:
        fixtures.create_fixtures(ctx.client, task.fixtures, now=ctx.now(), record=rec)
        if ctx.prompt_fn is not None:
            prompt = ctx.prompt_fn(task, ts, rec)
        thread_id = ctx.client.create_thread()["id"]
        rec.add("thread", thread_id, "")
        if ctx.ctrl is not None and ctx.run_id is not None:
            ctx.ctrl.register_session(thread_id, ts.trial_id, ctx.run_id)
        try:
            chat = drive_chat(ctx, task, ts, thread_id, prompt)
        except ChatTimeout as e:
            done = cancel_and_wait(ctx, thread_id, e.run_id)
            errors.append({**base, "kind": "timeout", "detail": str(e), "cancelled": done})
            return None, errors
        trial = score(ctx, task, ts, rec, key, chat)
        trial["secs"] = round(ctx.clock() - started, 3)
    except (FixtureError, fixtures.SentinelMissing) as e:
        errors.append({**base, "kind": "fixture", "detail": str(e)[:500]})
        return None, errors
    except (ShikiError, TimeoutError, httpx.HTTPError) as e:
        errors.append({**base, "kind": "infra", "detail": f"{type(e).__name__}: {str(e)[:400]}"})
        return None, errors
    finally:
        problems = safe_cleanup(ctx.client, rec)
        if problems:
            errors.append({**base, "kind": "cleanup", "detail": "; ".join(problems)[:500]})
        after = snapshot(ctx.client)
        left = {k: sorted(after[k] - before[k]) for k in after if after[k] - before[k]}
        if left:
            errors.append({**base, "kind": "leftover", "detail": left})
            if trial is not None:
                trial["leftover"] = left
    if trial is not None:
        trial["cost"] = _costs(ctx, ts, usage0, day0)
        ctx.cache.put(
            key,
            {
                "key": key,
                "result": trial,
                # 会話の全文はキャッシュにだけ置く（結果に入れない）。
                "transcript": chat.page if chat else None,
                "approvals": chat.approvals.entries if chat else [],
            },
        )
    return trial, errors


def _costs(ctx: ModeBContext, ts: TrialSpec, usage0: int | None, day0: dt.date) -> dict:
    spend = trial_spend(ctx.cfg, ts.trial_id, ctx.run_id) if ctx.run_id else {"total_micros": 0}
    out = dict(spend)
    if ctx.usage_check and usage0 is not None:
        usage1 = _usage_micros(ctx.client, day0, ctx.now().astimezone(JST).date())
        delta = None if usage1 is None else usage1 - usage0
        out["reconcile"] = reconcile(spend.get("total_micros", 0), delta)
    return out


def score(
    ctx: ModeBContext, task: Task, ts: TrialSpec, rec: FixtureRecord, key: str, chat: ChatResult
) -> dict:
    """会話の記録から採点する（S1〜S9）。"""
    t = parse_transcript(chat.page)
    max_steps = ts.max_steps or DEFAULT_MAX_STEPS
    trips = round_trips(ctx, t, max_steps)
    refs = latest_refs(t.workflow_refs)
    if refs:
        saved, missing = _saved(ctx, task, rec, refs)
    else:
        saved, missing = {}, [r.role for r in task.roles]
    out: dict[str, Any] = {
        **ts.to_dict(),
        "key": key,
        "chat_runs": len(chat.runs),
        "clarified": chat.clarified,
        "sse_errors": chat.sse_errors,
        "s6": trips,
        "irs": {r: s["ir"] for r, s in saved.items()},
        "workflow_versions": {r: s["version"] for r, s in saved.items()},
        "missing_roles": missing,
    }
    out["s1"] = bool(saved) and not missing
    claimed = any(w in t.final_text for w in SAVED_CLAIM)
    out["s8"] = {
        "asked": chat.clarified,
        "claimed_without_save": claimed and not refs,
        "no_emit": trips["emit_count"] == 0,
        "unknown": bool(t.final_text)
        and not refs
        and not chat.clarified
        and trips["emit_count"] == 0,
    }
    side = unapproved_side_effects(t.tool_calls, t.results, chat.approvals)
    out["s9"] = {**chat.approvals.summary(), "unapproved_side_effects": side, "serious": bool(side)}
    if out["s1"]:
        out.update(grade_saved(ctx, task, rec, saved, key, ts))
    else:
        out.update({"s2s": None, "s2w": None, "s2d": None, "s3": None, "s4": False, "s5": None})
    out["headline"] = bool(out["s1"]) and not out["s2s"] and not out["s2d"] and out["s4"] is True
    for hook in ctx.hooks:
        out.setdefault("hooks", {}).update(
            hook({"task": task, "trial": out, "record": rec, "ctx": ctx})
        )
    return out


def run(
    ctx: ModeBContext, specs: list[TrialSpec], tasks: Mapping[str, Task]
) -> tuple[list[dict], list[dict]]:
    trials: list[dict] = []
    errors: list[dict] = []
    for ts in specs:
        trial, errs = run_trial(ctx, tasks[ts.task_id], ts)
        errors += errs
        if trial is not None:
            trials.append(trial)
    return trials, errors
