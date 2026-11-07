"""Mode B（製品のチャットで作らせ、実行まで確かめる・GAP-30 §4.7）。並列度は 1。

1 試行の流れ:
1. fixture を作る（作った id は試行の記録に残す）。
2. `POST /threads` でスレッドを作り、abeval.proxy に thread_id（会話 ID）→ 試行を割り当てる。
3. `POST /threads/{id}/messages {text, model, effort}` を送る（stub では model `default` で effort を省く・
   Auto は model と effort を省く）。応答の run_id を控える。依頼文には Mode A と同じ参考情報（fixture の
   テーブル・フォルダー・ファイルの id と、入力の項目の key）を添える（チャットにはフォルダーやファイルの id を
   調べる道具が無いので、二つのモードで与える情報をそろえる。キャッシュのキーは印のままの文）。
4. 直後に SSE（`last_event_id=0`）を読み、`approval_requested` を承認の方針で決める（abeval.approvals・既定で
   却下）。SSE は承認の要求と終わりのイベント（done・error・status の done・failed・cancelled）を拾うために使い、
   完了は `GET messages` の `active_run_id` を `poll_secs` ごとに読み直して待つ（SSE が空で終われば読み直しに
   切り替える）。壁時計の上限（`wall_clock_secs`・6 分）を超えたら `POST …/runs/{run_id}/cancel` で取り消し、
   timeout として errors に入れる（採点しない）。
   - 終わりの確かめ（BUG-248）: 製品には run の状態を読む口が無い（`GET messages` にも状態は無い）。run が終わった
     のに終わりのイベントを読めていない（SSE が空で終わった・切れた）ときは、SSE を `last_event_id` から読み直す
     （製品の SSE は、スレッドの最新の run が終わっていれば残りのイベントを流して閉じる・`chat.rs` の
     `stream_thread`・`store/stream.rs`）。`END_REPLAYS` 回読み直しても読めなければ終わりは「確かめられない」とし、
     最後の user の発話の後に assistant の返事（中身のある発話）が無ければ失敗の印として、採点もキャッシュもせず
     errors（kind: no_reply）に入れる。返事があれば採点し、`chat_end_unconfirmed` に数を残す。終わった後に読んだ
     承認の要求には答えない（記録だけ残す）。
5. run が終わって、まだ保存していない（workflow_ref も成功した emit_workflow も無い）うえで最後の文章が
   聞き返しなら、課題の `clarifications` を 1 回だけ送る（S8 の asked は「保存の前に聞き返した」。保存の後の
   「有効化しますか？」は聞き返しに数えない・Mode A が emit を asked より先に判定するのと同じ順）。
6. `GET messages` で tool_call・tool_result・workflow_ref・最後の文章を回収する。emit_workflow の入力の IR は
   その版の `/workflows/validate` にかけ直して、検証エラーの回数とコードを数える（結果の文を読まない）。
7. 最後の workflow_ref の版を読み、workflow_set の課題はきっかけの種類で役割を割り当てる。
8. 静的に採点し、保存されたワークフローそのもので run_case を流す（評価の版は実モデル。費用は abeval.proxy の
   active で試行に割り当てる）。あわせて stub の配管も流す。
9. 費用を試行に割り当て（評価の台帳の行を試行で集める）、`/org-admin/ai/usage` の前後の差と突き合わせる。
10. 片付ける（記録した id と印付きのもの。試行の前に無かったものが残れば報告する）。

基盤の失敗は採点もキャッシュもせず errors に入れる（§4.8）: SSE の `error` イベントか失敗の状態で終わった会話
（kind: llm_error・上流の 429・5xx・送った後の誤りがあれば upstream）、abeval.proxy が上限で拒んだ要求と手元の
中継の 403（budget・残りの試行を打ち切る）、上流に繋がらない（upstream）、流せなかった run_case（run_case・
stub_plumbing）。拒否と繋がらない要求は台帳に載らないので、abeval.proxy の実行ごとの数（`GET /abeval/spend`）の
増え方で見る。各試行の前に、実行と評価の残りが 1 試行の最悪より少なければ打ち切る。

版のサーバーの確かめ直し（`ModeBContext.preflight`・BUG-246）: 各試行の発話の前と、採点した試行をキャッシュに
置く前に呼ぶ。:10067 のプロセスが run の前に確かめた版のサーバーのまま（anthropic が abeval.proxy・条件が同じ）で
なければ、その試行を採点もキャッシュもせず errors（kind: server）に入れ、残りを打ち切る。

キャッシュのキーには採点器の版（GRADER_VERSION と ir_lint の sha）と課題の期待（expect・roles・run_cases）の
sha も入れる（採点器か課題を直したら流し直す）。会話と採点を左右する条件（課題の `clarifications`・`ask_terms`、
承認の方針の `[mode_b] approve_tools` と副作用のあるツールの組、版のサーバーの 1 発話の予算、stub の配管を流すか、
採点の後の hook）も入れる（どれかを変えて流し直したら、前の条件の結果を返さない・BUG-249）。

予定の週（BUG-247）: モデルが見る「今日」は測った日になる（製品の `today_note` は run の作成時刻から作る）。測る日
（日本時間）が課題の `scheduled_at` の週（`grade.dates.scheduled_window`）に入る日は、結果が予定の日時で決まる課題
（`Task.scheduled_dependent`・`requires` の `cap:scheduled_at`）を流さずに errors（kind: date_window）に理由を
入れる（「今日」「先週」を決め打ちした IR も run_case を通り得るため）。`run` は計画の前に外す（`cli.cmd_run`）。
ここでも見るのは、流している間に日付が変わったときのため。stub のスタックの配管の確かめでは見ない（stub の
モデルは日付を決め打ちしない）。

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
from abeval.approvals import (
    SIDE_EFFECT_TOOLS,
    ApprovalLog,
    ApprovalPolicy,
    handle_request,
    unapproved_side_effects,
)
from abeval.budget import trial_spend
from abeval.cache import TrialCache, trial_key
from abeval.config import Config, canonical_json, sha256_hex
from abeval.fixtures import FixtureError, FixtureRecord
from abeval.grade.dates import in_window, scheduled_window
from abeval.grade.static import LintCatalog
from abeval.grading import (
    InfraFailure,
    Lint,
    codes,
    headline_b,
    raise_if_infra,
    run_saved,
    safe_cleanup,
    static_roles,
    stub_plumbing,
    validate_roles,
)
from abeval.pricing import micros_to_usd, usd_to_micros
from abeval.proxy_control import ProxyControl, ProxyControlError
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
    placeholder_ids,
    prompt_text,
)

JST = ZoneInfo("Asia/Tokyo")
DEFAULT_MAX_STEPS = 6
TERMINAL_EVENTS = frozenset({"done", "error"})
TERMINAL_STATUS = frozenset({"done", "failed", "cancelled"})
# run が終わったのに終わりのイベントを読めていないとき、SSE を読み直す回数（BUG-248）。
END_REPLAYS = 2
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
    # 採点器の版（GRADER_VERSION と ir_lint の sha の正規化した JSON の sha・キャッシュのキーに入れる）。
    grader_sha: str = "none"
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
    # 版のサーバーの確かめ直し（違えば理由・`ServerIdentity.recheck`・stub のスタックでは None）。
    preflight: Callable[[], str | None] | None = None
    # 版のサーバーの 1 発話の予算（マイクロ USD・`run --budget-micros`。版の環境と同じことは run の前に確かめる）。
    budget_micros: int | None = None


@dataclass
class ChatResult:
    runs: list[str]
    approvals: ApprovalLog
    clarified: bool
    sse_errors: list[str]
    page: dict
    # 会話が誤りで終わった印（SSE の error イベントの文・失敗の状態）。空なら誤りなし。
    failures: list[dict] = field(default_factory=list)
    # run → 終わりの状態（SSE の終わりのイベントの種類か、読めなければ "unconfirmed"・BUG-248）。
    ends: dict[str, str] = field(default_factory=dict)

    def unconfirmed(self) -> list[str]:
        return [r for r in self.runs if self.ends.get(r, "unconfirmed") == "unconfirmed"]


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


def _end_of(data: Mapping) -> str | None:
    """SSE のイベントが終わりのイベントなら、その種類（done・error・failed・cancelled）。"""
    kind = data.get("type")
    if kind in TERMINAL_EVENTS:
        return str(kind)
    if kind == "status" and data.get("status") in TERMINAL_STATUS:
        return str(data["status"])
    return None


def wait_run(
    ctx: ModeBContext,
    thread_id: str,
    run_id: str,
    log: ApprovalLog,
    deadline: float,
    errors: list,
    failures: list | None = None,
    ends: dict | None = None,
) -> dict:
    """run の終わりまで待つ（SSE で承認の要求と終わりのイベントを拾い、完了は読み直しで判定）。最後のメッセージ
    一覧を返す。

    SSE の `error` イベント（生成の失敗）と、失敗・取り消しの状態は `failures` に残す（基盤の失敗の判定に使う）。
    `ends[run_id]` に終わりの状態（終わりのイベントの種類か "unconfirmed"）を残す。run が終わったのに終わりの
    イベントを読めていなければ、SSE を `last_event_id` から `END_REPLAYS` 回まで読み直す（BUG-248）。
    """
    failures = [] if failures is None else failures
    ends = {} if ends is None else ends
    last_id = 0
    end: str | None = None
    finished = False  # GET messages で run の終わりを見た（この後の SSE は読み直し）
    replays = 0
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
                if data.get("type") == "approval_requested":
                    if finished:
                        # 終わった run の要求には答えない（決める相手がいない・記録だけ残す）。
                        call_id = data.get("tool_call_id")
                        if isinstance(call_id, str) and not log.seen(call_id):
                            log.add(data, run_id, False, "run は終わっていた（答えを送らない）")
                    else:
                        handle_request(ctx.client, thread_id, run_id, data, ctx.policy, log)
                    continue
                got = _end_of(data)
                if got is not None:
                    end = got
                    if got == "error":
                        failures.append(
                            {"run": run_id, "error": str(data.get("message") or "")[:300]}
                        )
                    elif got in ("failed", "cancelled"):
                        failures.append({"run": run_id, "status": got})
                    break
                if _remaining(ctx, deadline) <= 0:
                    break
        except (httpx.HTTPError, ShikiError) as e:
            errors.append(f"sse: {type(e).__name__}")
        if finished and end is None:
            replays += 1
        page = ctx.client.messages(thread_id)
        if not page.get("active_run_id") and not page.get("queued_runs"):
            if end is not None or replays >= END_REPLAYS:
                ends[run_id] = end or "unconfirmed"
                return page
            finished = True
            continue  # 終わった run の SSE の残りを読み直す（待たない）
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
    failures: list[dict] = []
    ends: dict[str, str] = {}
    args = message_args(ctx, ts)
    run_id = ctx.client.post_message(thread_id, prompt, **args)["run_id"]
    runs = [run_id]
    page = wait_run(ctx, thread_id, run_id, log, deadline, sse_errors, failures, ends)
    clarified = False
    first = parse_transcript(page)
    if (
        not failures
        and has_reply(page)
        and not saved_anything(first)
        and is_question(first.final_text, ask_terms(task))
    ):
        run_id = ctx.client.post_message(thread_id, task.clarifications, **args)["run_id"]
        runs.append(run_id)
        clarified = True
        page = wait_run(ctx, thread_id, run_id, log, deadline, sse_errors, failures, ends)
    return ChatResult(runs, log, clarified, sse_errors, page, failures, ends)


def _ordered(page: Mapping) -> list[Mapping]:
    """メッセージを作られた順に（同じ時刻なら一覧の順・`parse_transcript` と同じ並べ方）。"""
    msgs = [m for m in page.get("messages") or [] if isinstance(m, Mapping)]
    return [
        m
        for _, m in sorted(enumerate(msgs), key=lambda p: (str(p[1].get("created_at") or ""), p[0]))
    ]


def _substantive(block: object) -> bool:
    """返事の中身になるブロックか（思考と空の文は数えない）。"""
    if not isinstance(block, Mapping):
        return False
    kind = block.get("type")
    if kind == "thinking":
        return False
    if kind == "text":
        return bool(str(block.get("text") or "").strip())
    return kind is not None


def has_reply(page: Mapping) -> bool:
    """最後の user の発話の後に、中身のある assistant の発話があるか（製品は発話を受けると中身の無い枠を置く）。"""
    msgs = _ordered(page)
    users = [i for i, m in enumerate(msgs) if m.get("role") == "user"]
    after = msgs[users[-1] + 1 :] if users else msgs
    return any(
        m.get("role") == "assistant" and any(_substantive(b) for b in m.get("content") or [])
        for m in after
    )


def saved_anything(t: Transcript) -> bool:
    """その会話で保存したか（workflow_ref があるか、emit_workflow が成功した）。"""
    if t.workflow_refs:
        return True
    return any(
        c.get("name") == EMIT_TOOL and t.results.get(str(c.get("id")), {}).get("ok") is True
        for c in t.tool_calls
    )


# --- 記録の読み取り ---


def parse_transcript(page: Mapping) -> Transcript:
    """メッセージ一覧から tool_call・tool_result・workflow_ref・最後の文章を読む（知らないブロックは無視）。"""
    t = Transcript()
    for m in _ordered(page):
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


def task_expect_sha(task: Task) -> str:
    """課題の期待（expect・roles・run_cases）の sha（直したらキャッシュを使わない）。"""
    raw = task.raw if isinstance(task.raw, Mapping) else {}
    return sha256_hex(canonical_json({k: raw.get(k) for k in ("expect", "roles", "run_cases")}))


def key_prompt(task: Task, ts: TrialSpec) -> str:
    """キャッシュのキーの依頼文（参考情報の id は置き場所の印のまま）。"""
    return prompt_text(task, ts.phrasing, placeholder_ids(task), with_reference=True)


def key_conditions(ctx: ModeBContext, task: Task) -> dict:
    """会話と採点を左右する条件のうち、試行の条件（effort・手数）と版の sha のほかのもの（BUG-249）。"""
    return {
        "clarifications": task.clarifications,
        "ask_terms": list(ask_terms(task)),
        "approve_tools": sorted(ctx.policy.approve_tools),
        "side_effect_tools": sorted(SIDE_EFFECT_TOOLS),
        "budget_micros": ctx.budget_micros,
        "stub_plumbing": ctx.stub_plumbing,
        "hooks": [getattr(h, "__qualname__", type(h).__name__) for h in ctx.hooks],
    }


def key_for(ctx: ModeBContext, task: Task, ts: TrialSpec, prompt: str | None = None) -> str:
    cond = {**ts.condition(), "stub": ctx.stub}
    return trial_key(
        task_id=task.id,
        mode="b",
        phrasing=ts.phrasing,
        prompt=prompt if prompt is not None else key_prompt(task, ts),
        fixtures=task.fixtures,
        trial=ts.n,
        condition=cond,
        config_sha=ctx.config_sha,
        capture_sha=ctx.capture_sha,
        binary_sha=ctx.binary_sha,
        extra={
            "grader": ctx.grader_sha,
            "task_expect": task_expect_sha(task),
            "conditions": key_conditions(ctx, task),
        },
    )


def date_window_reason(task: Task, now: dt.datetime, *, stub: bool = False) -> str | None:
    """結果が予定の日時で決まる課題で、測る日が予定の週に入れば流さない理由（BUG-247）。stub では見ない。"""
    if stub or not task.scheduled_dependent:
        return None
    window = scheduled_window(task.scheduled_at())
    if window is None or not in_window(now, window):
        return None
    day = now.astimezone(JST).date()
    return (
        f"測る日 {day}（日本時間）が課題の予定の週（{window[0]}〜{window[1]}）に重なるので流しません"
        "（モデルが見る「今日」が予定の日と同じ週で、日付を決め打ちした IR も run_case を通り得る）"
    )


# --- 基盤の失敗の見分け ---


def proxy_counters(ctx: ModeBContext) -> dict | None:
    """abeval.proxy の、この実行の数（拒否・繋がらない・累計・上限）。中継を使わないなら None。"""
    if ctx.ctrl is None or ctx.run_id is None:
        return None
    s = ctx.ctrl.spend()
    r = (s.get("runs") or {}).get(ctx.run_id) or {}
    return {
        "rejected": int(r.get("rejected") or 0),
        "unreachable": int(r.get("unreachable") or 0),
        "last_rejection": r.get("last_rejection"),
        "run_left_micros": int(r.get("cap_micros") or 0)
        - int(r.get("spent_micros") or 0)
        - int(r.get("reserved_micros") or 0),
        "eval_left_micros": int(s.get("eval_cap_micros") or 0)
        - int(s.get("eval_spent_micros") or 0)
        - int(s.get("reserved_micros") or 0),
        "registered": bool(r),
    }


def budget_short(ctx: ModeBContext, ts: TrialSpec, counters: dict | None) -> str | None:
    """実行か評価の残りが 1 試行の最悪より少なければ理由（試行の前に見る）。"""
    if counters is None:
        return None
    if not counters["registered"]:
        return f"abeval.proxy に実行 {ctx.run_id} が登録されていません"
    worst = usd_to_micros(ctx.cfg.estimates["b"].worst_for(ts.effort, ts.max_steps))
    for name, left in (
        ("実行", counters["run_left_micros"]),
        ("評価", counters["eval_left_micros"]),
    ):
        if left < worst:
            return (
                f"abeval: {name}の残り ${micros_to_usd(max(left, 0)):.4f} が 1 試行の最悪 "
                f"${micros_to_usd(worst):.4f} より少ない"
            )
    return None


def infra_failure(
    ctx: ModeBContext,
    ts: TrialSpec,
    before: dict | None,
    chat: ChatResult | None,
    trial: dict | None = None,
) -> dict | None:
    """この試行の中で基盤の失敗があったか（{kind, detail, stop}）。無ければ None。"""
    now = proxy_counters(ctx) if before is not None else None
    if now is not None and before is not None and now["rejected"] > before["rejected"]:
        return {"kind": "budget", "detail": str(now["last_rejection"])[:300], "stop": True}
    sp = trial_spend(ctx.cfg, ts.trial_id, ctx.run_id) if ctx.run_id else {}
    st = sp.get("statuses") or {}
    chat_st, case_st = st.get("chat") or {}, st.get("run_case") or {}
    if "403" in chat_st or "403" in case_st:
        return {"kind": "budget", "detail": "手元の中継が 403 を返した（総額の上限）", "stop": True}
    if now is not None and before is not None and now["unreachable"] > before["unreachable"]:
        return {"kind": "upstream", "detail": "abeval.proxy が上流に繋がらない", "stop": False}
    if chat is not None and chat.failures:
        kind = "upstream" if chat_st else "llm_error"
        detail = f"会話が誤りで終わった: {chat.failures[:3]}" + (
            f"・上流の応答 {chat_st}" if chat_st else ""
        )
        return {"kind": kind, "detail": detail[:500], "stop": False}
    if chat is not None and chat.unconfirmed() and not has_reply(chat.page):
        # 終わりの状態を確かめられず、最後の発話に返事が無い（製品の中の失敗とみなす・BUG-248）。
        detail = (
            f"run {chat.unconfirmed()} の終わりを SSE で確かめられず、最後の発話に返事がありません"
            + (f"・上流の応答 {chat_st}" if chat_st else "")
        )
        return {"kind": "no_reply", "detail": detail[:500], "stop": False}
    if trial is not None and case_st and trial.get("s3") is False:
        return {
            "kind": "upstream",
            "detail": f"run_case の LLM の呼び出しに 200 以外の応答 {case_st}",
            "stop": False,
        }
    return None


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
    raise_if_infra("run_case", runs)
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
        raise_if_infra("stub_plumbing", stub)
        out["stub"] = stub
        if stub.get("s2d") is not None:
            s2d = bool(s2d) or stub["s2d"]
    out["s2d"] = s2d
    return out


def run_trial(ctx: ModeBContext, task: Task, ts: TrialSpec) -> tuple[dict | None, list[dict]]:
    """1 試行（結果は採点した記録、基盤の失敗なら None）。(記録, errors)。

    errors の `stop: true` は、残りの試行を打ち切る印（費用の上限・abeval.proxy に届かない）。
    """
    key = key_for(ctx, task, ts)
    cached = ctx.cache.get(key)
    if cached and isinstance(cached.get("result"), dict):
        return {**cached["result"], "from_cache": True}, []
    errors: list[dict] = []
    base = {**ts.to_dict(), "key": key, "mode": "b"}
    dated = date_window_reason(task, ctx.now(), stub=ctx.stub)
    if dated:
        return None, [{**base, "kind": "date_window", "detail": dated}]
    why = ctx.preflight() if ctx.preflight is not None else None
    if why:
        return None, [{**base, "kind": "server", "detail": why, "stop": True}]
    try:
        counters = proxy_counters(ctx)
    except (ProxyControlError, httpx.HTTPError) as e:
        return None, [{**base, "kind": "proxy", "detail": f"{type(e).__name__}", "stop": True}]
    short = budget_short(ctx, ts, counters)
    if short:
        return None, [{**base, "kind": "budget", "detail": short, "stop": True}]
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
        else:
            prompt = prompt_text(task, ts.phrasing, rec.refs, with_reference=True)
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
        infra = infra_failure(ctx, ts, counters, chat)
        if infra is None:
            trial = score(ctx, task, ts, rec, key, chat)
            infra = infra_failure(ctx, ts, counters, chat, trial)
        if infra is not None:
            trial = None  # 採点もキャッシュもしない
            errors.append({**base, **infra, "cost": _costs(ctx, ts, usage0, day0)})
            return None, errors
        trial["secs"] = round(ctx.clock() - started, 3)
    except (FixtureError, fixtures.SentinelMissing) as e:
        errors.append({**base, "kind": "fixture", "detail": str(e)[:500]})
        return None, errors
    except InfraFailure as e:
        # 採点しない（部分だけで採点しない・キャッシュにも置かない）。使った費用は残す。
        errors.append(
            {**base, "kind": e.kind, "detail": e.detail, "cost": _costs(ctx, ts, usage0, day0)}
        )
        return None, errors
    except (ShikiError, TimeoutError, httpx.HTTPError, ProxyControlError) as e:
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
    why = ctx.preflight() if trial is not None and ctx.preflight is not None else None
    if why:
        # 流している間に版のサーバーが替わった（採点もキャッシュもしない）。
        errors.append({**base, "kind": "server", "detail": why, "stop": True})
        return None, errors
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
        # stub のスタックの配管の確かめ（集計で実モデルの条件に混ぜない）。
        "stub_run": ctx.stub,
        "chat_runs": len(chat.runs),
        # 終わりを SSE で確かめられなかった run の数（返事はあったので採点した・BUG-248）。
        "chat_end_unconfirmed": len(chat.unconfirmed()),
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
    out["headline"] = headline_b(out["s1"], out["s2s"], out["s2d"], out["s4"])
    for hook in ctx.hooks:
        out.setdefault("hooks", {}).update(
            hook({"task": task, "trial": out, "record": rec, "ctx": ctx})
        )
    return out


def run(
    ctx: ModeBContext, specs: list[TrialSpec], tasks: Mapping[str, Task]
) -> tuple[list[dict], list[dict]]:
    """試行の列を順に流す（`stop` の印の付いた基盤の失敗が出たら、残りを stopped として打ち切る）。"""
    trials: list[dict] = []
    errors: list[dict] = []
    stopped: str | None = None
    for ts in specs:
        if stopped:
            errors.append({**ts.to_dict(), "kind": "stopped", "detail": stopped})
            continue
        trial, errs = run_trial(ctx, tasks[ts.task_id], ts)
        errors += errs
        if trial is not None:
            trials.append(trial)
        hit = next((e for e in errs if e.get("stop")), None)
        if hit:
            stopped = str(hit.get("detail") or hit.get("kind"))
    return trials, errors
