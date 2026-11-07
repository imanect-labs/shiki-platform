"""`python -m abeval capture|plan|run|grade|report|budget` の中身（GAP-30 §4・§6・§8）。

- `plan`: 計画と見積もりを出すだけ（費用を使わない）。
- `run --mode a|b`: 既定は計画を出して終わる。`--go` を付けたときだけ、点検（doctor）→ abeval.proxy への実行の
  登録 → 取り込み → 試行 → 結果の書き出し、の順に進む（登録の上限は min(最悪 × 1.2, $10)）。
  - stub でない `run` と `capture` は、計画を出す前に必ず版のサーバーを確かめる（`--skip-doctor` でも飛ばさない・
    BUG-246）: :10067 を握るプロセスが `--label` の版のバイナリ（sha が版の記録と同じ）で、anthropic が abeval.proxy を
    指し、Mode B では手数の上限と 1 発話の予算が `--max-steps`・`--budget-micros` と同じこと（`doctor.version_guard`）。
    あわせて振る舞いの指紋（`behavior.verify`）が serve で記録したものと同じこと。確かめたプロセスは、取り込みの前・
    Mode B の各試行の前と結果を残す前・結果を書く前に、替わっていないか確かめ直す（`ServerIdentity.recheck`）。
  - Mode B は、測る日が予定の週に入る課題（`mode_b.date_window_reason`）を、版に無い機能の課題（対象外）と同じく
    計画の前に外す（登録の予約と $10 の判定に入れない）。外した課題は試行ごとに errors（kind: date_window）に残す。
  - `--stub`（Mode B だけ）: stub のスタックで配管を確かめる（model `default`・費用 0・abeval.proxy を使わない）。
  - `--regrade-from <版>`（Mode A だけ）: 生成せず、その版の生成をこの版のサーバーで採点し直す（V1・費用 0）。
    取り込みの sha が同じことを条件にする。
- `grade --exp E`: 結果の IR を ir_lint（HEAD）で採点し直す（サーバー不要・S2s・S2w・S5 だけ）。
- `report`: results/*.json から summary.md を作る。`budget`: 費用の累計と残り。
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from abeval import behavior, doctor, fixtures, mode_a, mode_b, report
from abeval import budget as budget_mod
from abeval.anthropic_client import AnthropicClient
from abeval.approvals import ApprovalPolicy
from abeval.cache import TrialCache
from abeval.capture import Capture, CaptureError, capture, captures_dir
from abeval.config import Config, canonical_json, load_config, sha256_hex
from abeval.gen_checks import g9_supported, load_denylist, version_features
from abeval.grade.static import LintCatalog, grader_version, load_catalog, run_ir_lint
from abeval.ledger import Ledger
from abeval.plan import RunSpec, format_plan, make_plan, mode_a_reserve_usd, register, spent
from abeval.pricing import micros_to_usd
from abeval.proxy_control import ProxyControl, read_token, safe_id
from abeval.server_guard import Conditions, ServerIdentity
from abeval.shiki import ShikiClient
from abeval.tasks import PHRASINGS, Task, load_tasks
from abeval.trial import placeholder_ids, prompt_text
from abeval.versions import (
    DEFAULT_BUDGET_MICROS,
    LLM_EVAL_CONFIG,
    VersionError,
    probe_node_types,
    read_record,
    update_record,
)

STUB_URL = "http://localhost:8080"


def _lint(bin_path: Path | None = None):
    def lint(items):
        return run_ir_lint(items, bin_path)[0]

    return lint


@dataclass
class Version:
    label: str
    commit: str | None
    binary_sha: str
    extra: dict


def version_info(label: str, *, stub: bool) -> Version:
    if stub:
        return Version(label, None, "stub-stack", {})
    rec = read_record(label)
    return Version(label, rec.commit, rec.binary_sha256, dict(rec.extra))


def select_tasks(all_tasks: list[Task], names: str | None) -> list[Task]:
    if not names or names == "all":
        return list(all_tasks)
    want = [n.strip() for n in names.split(",") if n.strip()]
    by_id = {t.id: t for t in all_tasks}
    missing = [n for n in want if n not in by_id]
    if missing:
        raise SystemExit(f"課題がありません: {missing}")
    return [by_id[n] for n in want]


def in_scope(
    tasks: list[Task], catalog: LintCatalog, caps: Mapping, node_types: list[str] | None = None
) -> tuple[list[Task], list[dict]]:
    """版に無い機能の要る課題を外す（対象外として数える）。ノードの種類は版のサーバーで確かめたもの。"""
    triggers = (
        ("interactive", "schedule", "event") if caps.get("trigger") else ("interactive", "schedule")
    )
    feats = version_features(catalog, caps, triggers=triggers, node_types=node_types)
    ok, out = [], []
    for t in tasks:
        g = g9_supported(t, feats)
        if g.ok:
            ok.append(t)
        else:
            out.append({"task": t.id, "missing": g.problems})
    return ok, out


def display_note(text: str | None) -> str | None:
    """結果に残す注記（ホームの絶対パスを `~` にする・利用者名を残さない）。"""
    if not text:
        return None
    return text.replace(str(Path.home()), "~")


def _record_node_types(label: str, node_types: list[str]) -> None:
    """版の記録に、版のサーバーで確かめたノードの種類を残す（記録の無い版は残さない）。"""
    try:
        rec = read_record(label)
    except VersionError:
        return
    if rec.extra.get("node_types") != node_types:
        update_record(label, {"node_types": node_types})


def split_date_window(tasks: list[Task], now: dt.datetime) -> tuple[list[Task], dict[str, str]]:
    """Mode B で、測る日が予定の週に入る課題を外す（(流す課題, 外した課題 → 理由)・BUG-247）。"""
    kept: list[Task] = []
    reasons: dict[str, str] = {}
    for t in tasks:
        why = mode_b.date_window_reason(t, now)
        if why:
            reasons[t.id] = why
        else:
            kept.append(t)
    return kept, reasons


def date_window_errors(args, windowed: list[Task], reasons: Mapping[str, str]) -> list[dict]:
    """計画から外した課題の試行ごとの errors（kind: date_window・レポートで条件ごとに外した課題を数える）。"""
    if not windowed:
        return []
    return [
        {**ts.to_dict(), "kind": "date_window", "detail": reasons[ts.task_id]}
        for ts in _spec(args, windowed).trial_specs()
    ]


def _spec(args, tasks: list[Task]) -> RunSpec:
    efforts = tuple(None if e == "auto" else e for e in (args.effort or ["medium"]))
    phrasings = tuple(args.phrasings.split(",")) if args.phrasings else PHRASINGS
    return RunSpec(
        exp=args.exp,
        mode=args.mode,
        label=args.label,
        tasks=tuple(t.id for t in tasks),
        phrasings=phrasings,
        trials=args.trials,
        efforts=efforts,
        max_steps=args.max_steps,
    )


def latest_capture(label: str) -> Capture | None:
    root = captures_dir() / label
    best = None
    for meta in root.glob("*/meta.json"):
        try:
            at = json.loads(meta.read_text(encoding="utf-8")).get("captured_at", "")
        except ValueError:
            continue
        if best is None or at > best[0]:
            best = (at, meta.parent.name)
    if best is None:
        return None
    try:
        return Capture.read(label, best[1])
    except CaptureError:
        return None


def _reserve(cfg: Config, cap: Capture | None, tasks: list[Task]) -> float | None:
    if cap is None:
        return None
    longest = max(
        (
            len(prompt_text(t, p, placeholder_ids(t), with_reference=True).encode())
            for t in tasks
            for p in PHRASINGS
        ),
        default=0,
    )
    body = len(cap.system.encode()) + len(json.dumps(cap.emit_tool(), ensure_ascii=False).encode())
    return mode_a_reserve_usd(cfg, body + longest + 2048, cap.max_tokens)


def _capture_task(tasks: list[Task]) -> Task | None:
    return next((t for t in tasks if t.fixtures.get("tables")), None)


def do_capture(
    client: ShikiClient,
    ctrl: ProxyControl,
    cfg: Config,
    label: str,
    exp: str,
    task: Task | None,
    llm_config: Path | None,
) -> Capture:
    """fixture を作ってから取り込む（data のツールが出るように・片付けまでする）。"""
    rec = fixtures.create_fixtures(client, task.fixtures) if task else None
    try:
        ids = {}
        names = {}
        if task and rec:
            kinds = task.fixture_refs()
            ids = {rec.refs[r]: f"@{kinds[r]}:{r}" for r in kinds if r in rec.refs}
            names = {
                k: sorted(str(x["name"]) for x in task.fixtures.get(k) or [])
                for k in ("tables", "folders", "csv")
            }
        return capture(
            client,
            ctrl,
            cfg,
            label=label,
            exp=exp,
            llm_config=llm_config,
            fixture_ids=ids,
            fixture_names=names,
        )
    finally:
        if rec is not None:
            fixtures.cleanup(client, rec)


def _versions_entry(
    v: Version, cap: Capture | None, caps: Mapping, node_types: list[str] | None = None
) -> dict:
    fp = (cap.meta.get("fingerprint") or {}) if cap else {}
    return {
        "node_types": node_types,
        "commit": v.commit,
        "binary_sha256": v.binary_sha,
        "config_sha": fp.get("llm_config_sha"),
        "capture_sha": cap.sha if cap else None,
        "fingerprint_sha": cap.meta.get("fingerprint_sha") if cap else None,
        "capture_changed": cap.meta.get("changed_from_previous") if cap else None,
        "capabilities": dict(caps),
        "behavior_check": v.extra.get("behavior_check"),
    }


def cmd_plan(args, *, go_allowed: bool = False) -> tuple[int, object]:
    cfg = load_config()
    tasks = select_tasks(load_tasks(), args.tasks)
    spec = _spec(args, tasks)
    eval_usd, relay_usd, notes = spent(cfg)
    reserve = _reserve(cfg, latest_capture(args.label), tasks) if args.mode == "a" else None
    plan = make_plan(
        spec,
        cfg,
        eval_spent_usd=eval_usd,
        relay_spent_usd=relay_usd,
        reserve_usd=reserve,
        notes=notes,
    )
    print(format_plan(plan))
    return (0 if plan.ok else 2), plan


def cmd_run(args) -> int:
    cfg = load_config()
    stub = bool(getattr(args, "stub", False))
    if stub and args.mode != "b":
        raise SystemExit("--stub は Mode B だけです")
    if stub:
        args.effort = ["auto"]  # stub は effort を受けない（条件の印も auto にする）
    api = (
        args.api_url or os.environ.get("ABEVAL_API_URL") or (STUB_URL if stub else cfg.api_url)
    ).rstrip("/")
    all_tasks = select_tasks(load_tasks(), args.tasks)
    # 版のサーバーの点検は何よりも先に（--skip-doctor でも飛ばさない・BUG-246）。
    ident = None if stub else guard_version(api, cfg, args, mode_b=args.mode == "b")
    if not stub and ident is None:
        return 1
    lint = _lint(Path(args.lint_bin) if args.lint_bin else None)
    catalog = load_catalog()
    client = ShikiClient(api, "alice", stub_only=stub, timeout=60.0)
    try:
        if not stub and not guard_behavior(args.label, client):
            return 1
        if fixtures.find_sentinel(client) is None:
            print(
                f"番兵（alice のルート直下のフォルダー「{fixtures.SENTINEL_NAME}」）がありません。評価用のサーバーなら "
                "python -m abeval.doctor --init-sentinel で作ってから流します"
            )
            return 1
        caps = client.capabilities()
        # requires の node:* は、HEAD の ir_lint のカタログではなく、版のサーバーの検証で確かめる（cap:* と同じく版で）。
        node_types = probe_node_types(client, sorted(catalog.nodes))
        if not stub:
            _record_node_types(args.label, node_types)
        tasks, out_of_scope = in_scope(all_tasks, catalog, caps, node_types)
        for o in out_of_scope:
            print(f"[対象外] {o['task']}: {o['missing']}")
        window: dict[str, str] = {}
        windowed: list[Task] = []
        if args.mode == "b" and not stub:
            # 予定の週に流さない課題は、対象外と同じく計画の前に外す（登録の予約と $10 の判定に入れない）。
            tasks, window = split_date_window(tasks, fixtures.now_utc())
            windowed = [t for t in all_tasks if t.id in window]
            for t in windowed:
                print(
                    f"[日付] {t.id}: {window[t.id]}（計画から外し、errors に date_window で残す）"
                )
        if not tasks:
            print("流せる課題がありません")
            return 2
        args_tasks = ",".join(t.id for t in tasks)
        args.tasks = args_tasks
        code, plan = cmd_plan(args)
        if stub:
            print("[注] --stub は費用を使わない（見積もりは参考・abeval.proxy に登録しない）")
        regrade = getattr(args, "regrade_from", None)
        if not args.go and not stub and not regrade:
            print("（--go を付けたときだけ実行する）")
            return code
        if code != 0 and not stub and not regrade:
            return code
        version = version_info(args.label, stub=stub)
        ctrl = None
        cap = None
        try:
            if not stub and not regrade:
                if not args.skip_doctor:
                    checks = doctor.run_checks(
                        mode="eval", api_url=api, cfg=cfg, llm_config=Path(args.llm_config)
                    )
                    bad = [c for c in checks if c.status == "ng"]
                    for c in bad:
                        print(c.line())
                    if bad:
                        print("点検が通らないので実行しません")
                        return 1
                ctrl = ProxyControl(
                    f"http://{cfg.proxy.host}:{cfg.proxy.port}", read_token(cfg.paths.home)
                )
                reg = register(plan, ctrl)
                print(f"abeval.proxy に登録: {reg}")
            if not stub:
                if not recheck_server(ident, "取り込みの前"):
                    return 1
                if ctrl is None:
                    ctrl = ProxyControl(
                        f"http://{cfg.proxy.host}:{cfg.proxy.port}", read_token(cfg.paths.home)
                    )
                cap = do_capture(
                    client,
                    ctrl,
                    cfg,
                    args.label,
                    safe_id(args.exp),
                    _capture_task(tasks),
                    Path(args.llm_config).expanduser(),
                )
                if cap.meta.get("changed_from_previous"):
                    print(f"[注] 前の取り込みと違う項目: {cap.meta['changed_from_previous']}")
            trials, errors = _execute(
                args, cfg, client, ctrl, plan, version, cap, tasks, lint, catalog, stub, regrade,
                preflight=None if ident is None else ident.recheck,
            )  # fmt: skip
            errors = [*date_window_errors(args, windowed, window), *errors]
        finally:
            if ctrl is not None:
                if args.go and not stub and not regrade:
                    ctrl.remove_run(plan.run_id)
                ctrl.close()
        exp_usd = micros_to_usd(
            sum(
                e["cost_micros"]
                for e in Ledger(cfg.paths.ledger).entries()
                if e.get("run_id") == plan.run_id
            )
        )
        if not recheck_server(ident, "結果を書く前"):
            return 1
        _, g8_skipped = load_denylist()
        if g8_skipped:
            print(f"[注] {g8_skipped}（結果に g8_skipped の印を残す）")
        doc = report.build_result(
            exp=args.exp,
            trials=trials,
            runs=[
                {
                    **plan.to_dict(),
                    "finished_at": dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
                    "stub": stub,
                    "regrade_from": regrade,
                    # 拒否リスト（G8）が無くて、課題・結果の検査を飛ばしたか（理由の文・無ければ None）。
                    "g8_skipped": display_note(g8_skipped),
                }
            ],
            versions={args.label: _versions_entry(version, cap, caps, node_types)},
            grader=grader_version(Path(args.lint_bin) if args.lint_bin else None),
            cost={"exp_usd": exp_usd},
            out_of_scope=[{**o, "label": args.label} for o in out_of_scope],
        )
        paths = report.write_result(doc, errors)
        print(
            f"書きました: {paths[0]}（{len(trials)} 試行）・{paths[1]}（基盤の失敗 {len(errors)} 件）"
        )
        return 0
    finally:
        client.close()


def _execute(
    args, cfg, client, ctrl, plan, version, cap, tasks, lint, catalog, stub, regrade, preflight=None
):
    by_id = {t.id: t for t in tasks}
    cache = TrialCache(cfg.paths.cache)
    if args.mode == "a":
        key_sha = version.binary_sha
        if regrade:
            src = version_info(regrade, stub=False)
            key_sha = src.binary_sha
            prev = latest_capture(regrade)
            if prev is None or cap is None or prev.sha != cap.sha:
                raise SystemExit(
                    f"取り込みの sha が {regrade} と違うので採点し直しません（説明文が違う）"
                )
        ctx = mode_a.ModeAContext(
            cfg=cfg,
            client=client,
            anthropic=None
            if regrade
            else AnthropicClient(
                f"http://{cfg.proxy.host}:{cfg.proxy.port}",
                session_header=cfg.proxy.session_header,
                max_attempts=cfg.mode_a.max_attempts,
            ),
            capture=cap,
            lint=lint,
            catalog=catalog,
            cache=cache,
            binary_sha=key_sha,
            config_sha=(cap.meta.get("fingerprint") or {}).get("llm_config_sha") or "none",
            ctrl=ctrl,
            run_id=None if regrade else plan.run_id,
            generate=not regrade,
            stub_plumbing=not args.no_stub_plumbing,
        )
        out = mode_a.run(ctx, plan.trials, by_id)
        if out.stopped:
            print(f"[打ち切り] {out.stopped}")
        return out.trials, out.errors
    ctx = mode_b.ModeBContext(
        cfg=cfg,
        client=client,
        policy=ApprovalPolicy(frozenset(cfg.mode_b.approve_tools)),
        lint=lint,
        catalog=catalog,
        cache=cache,
        binary_sha=version.binary_sha,
        config_sha=((cap.meta.get("fingerprint") or {}).get("llm_config_sha") if cap else None)
        or "stub",
        capture_sha=cap.sha if cap else "stub",
        grader_sha=sha256_hex(
            canonical_json(grader_version(Path(args.lint_bin) if args.lint_bin else None))
        ),
        ctrl=None if stub else ctrl,
        run_id=None if stub else plan.run_id,
        stub=stub,
        stub_plumbing=not args.no_stub_plumbing,
        preflight=preflight,
        budget_micros=args.budget_micros,
    )
    trials, errors = mode_b.run(ctx, plan.trials, by_id)
    stopped = next((e["detail"] for e in errors if e.get("kind") == "stopped"), None)
    if stopped:
        print(f"[打ち切り] {stopped}")
    return trials, errors


REFUSE = (
    "送り先が --label の版のサーバー（anthropic が abeval.proxy）と確かめられるまで送りません"
    "（python -m abeval serve --label <版> で起動する）"
)


def guard_version(api: str, cfg: Config, args, *, mode_b: bool) -> ServerIdentity | None:
    """run・capture の前に必ず通す点検（--skip-doctor でも飛ばさない・doctor.version_guard・BUG-246）。"""
    cond = Conditions(args.max_steps, args.budget_micros) if mode_b else None
    checks, ident = doctor.version_guard(
        api, cfg, args.label, llm_config=Path(args.llm_config).expanduser(), conditions=cond
    )
    for c in checks:
        if c.status == "ng":
            print(c.line())
    if ident is None:
        print(REFUSE)
    return ident


def guard_behavior(label: str, client: ShikiClient) -> bool:
    """振る舞いの指紋が serve で記録したものと同じか（違えば流さない・§4.1 の 5）。"""
    why = behavior.verify(label, client)
    if why:
        print(f"[NG]   版の振る舞いの指紋: {why}")
        print(REFUSE)
    return why is None


def recheck_server(ident: ServerIdentity | None, when: str) -> bool:
    """確かめた版のサーバーのままか（stub は確かめない）。"""
    why = ident.recheck() if ident is not None else None
    if why:
        print(f"[NG]   {when}の確かめ直し: {why}")
        print(REFUSE)
    return why is None


def cmd_capture(args) -> int:
    cfg = load_config()
    api = (args.api_url or os.environ.get("ABEVAL_API_URL") or cfg.api_url).rstrip("/")
    ident = guard_version(api, cfg, args, mode_b=False)
    if ident is None:
        return 1
    tasks = load_tasks()
    task = select_tasks(tasks, args.task)[0] if args.task else _capture_task(tasks)
    with ShikiClient(api, "alice", timeout=60.0) as client:
        if not guard_behavior(args.label, client):
            return 1
        ctrl = ProxyControl(f"http://{cfg.proxy.host}:{cfg.proxy.port}", read_token(cfg.paths.home))
        with ctrl:
            cap = do_capture(
                client,
                ctrl,
                cfg,
                args.label,
                safe_id(args.exp),
                task,
                Path(args.llm_config).expanduser(),
            )
    print(
        f"取り込みました: data/captures/{args.label}/{args.exp}（sha {cap.sha[:12]}・tools {len(cap.tools)}）"
    )
    if cap.meta.get("changed_from_previous"):
        print(f"[注] 前の取り込みと違う項目: {cap.meta['changed_from_previous']}")
    return 0


def cmd_grade(args) -> int:
    """結果の IR を ir_lint で採点し直す（S2s・S2w・S5・見出しの S2s の部分）。"""
    from abeval.grading import headline, headline_b, static_roles

    doc = report.read_result(args.exp)
    if doc is None:
        raise SystemExit(f"結果がありません: results/{args.exp}.json")
    tasks = {t.id: t for t in load_tasks()}
    lint = _lint(Path(args.lint_bin) if args.lint_bin else None)
    catalog = load_catalog()
    n = 0
    for t in doc.get("trials") or []:
        irs = t.get("irs") or {}
        task = tasks.get(t.get("task"))
        if not irs or task is None:
            continue
        st = static_roles(task, irs, lint, catalog, str(t["key"])[:12])
        t["static"] = {k: v for k, v in st.items() if k != "node_ports"}
        t["s2s"], t["s2w"], t["s5"] = st["s2s"], st["s2w"], st["s5"]
        if t.get("mode") == "b":
            t["headline"] = headline_b(t.get("s1"), t["s2s"], t.get("s2d"), t.get("s4"))
        else:
            t["headline"] = headline(t.get("s1"), t["s2s"], t.get("s2d"))
        n += 1
    doc["grader"] = grader_version(Path(args.lint_bin) if args.lint_bin else None)
    report.write_result(doc, [], merge=False)
    print(f"採点し直しました: {n} 試行（results/{args.exp}.json）")
    return 0


def cmd_report(args) -> int:
    cfg = load_config()
    names = args.exp or sorted(p.stem for p in report.results_dir().glob("*.json"))
    docs = [d for d in (report.read_result(n) for n in names) if d]
    b = budget_mod.summarize(cfg).to_dict()
    path = report.write_summary(docs, budget=b)
    print(f"書きました: {path}（実験 {len(docs)}）")
    return 0


def _run_args(ap: argparse.ArgumentParser) -> None:
    ap.add_argument("--mode", choices=("a", "b"), required=True)
    ap.add_argument("--label", required=True, help="版のラベル（data/versions/<label>.json）")
    ap.add_argument("--exp", required=True, help="実験の名前（results/<exp>.json）")
    ap.add_argument("--tasks", help="課題の id（, 区切り・既定は全部）")
    ap.add_argument("--phrasings", help=f"言い方（, 区切り・既定 {','.join(PHRASINGS)}）")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument(
        "--effort",
        action="append",
        help="low|medium|high|auto（重ねて指定できる・既定 medium。auto は Mode A では思考なし、"
        "Mode B では製品の Auto）",
    )
    ap.add_argument(
        "--max-steps",
        type=int,
        help="手数の上限（Mode B は版のサーバーの起動の値と同じことを確かめる・serve --max-steps）",
    )
    ap.add_argument(
        "--budget-micros",
        type=int,
        default=DEFAULT_BUDGET_MICROS,
        help="1 発話の予算（マイクロ USD・Mode B は版のサーバーの起動の値と同じことを確かめる・"
        f"serve --budget-micros・既定 {DEFAULT_BUDGET_MICROS}）",
    )


def main(argv: list[str] | None = None, command: str | None = None) -> int:
    ap = argparse.ArgumentParser(prog=f"python -m abeval {command or ''}".strip())
    if command in ("plan", "run"):
        _run_args(ap)
    if command == "run":
        ap.add_argument(
            "--go", action="store_true", help="費用を使って実行する（無ければ計画だけ）"
        )
        ap.add_argument(
            "--stub", action="store_true", help="Mode B を stub のスタックで流す（費用 0）"
        )
        ap.add_argument(
            "--regrade-from", help="Mode A: この版の生成を採点し直す（生成しない・費用 0）"
        )
        ap.add_argument("--api-url")
        ap.add_argument("--llm-config", default=str(LLM_EVAL_CONFIG))
        ap.add_argument("--lint-bin")
        ap.add_argument("--skip-doctor", action="store_true")
        ap.add_argument("--no-stub-plumbing", action="store_true")
    if command == "capture":
        ap.add_argument("--label", required=True)
        ap.add_argument("--exp", required=True)
        ap.add_argument(
            "--task", help="取り込みの前に fixture を作る課題（既定はテーブルのある最初の課題）"
        )
        ap.add_argument("--api-url")
        ap.add_argument("--llm-config", default=str(LLM_EVAL_CONFIG))
    if command == "grade":
        ap.add_argument("--exp", required=True)
        ap.add_argument("--lint-bin")
    if command == "report":
        ap.add_argument("--exp", action="append")
    args = ap.parse_args(argv)
    try:
        if command == "plan":
            return cmd_plan(args)[0]
        if command == "run":
            return cmd_run(args)
        if command == "capture":
            return cmd_capture(args)
        if command == "grade":
            return cmd_grade(args)
        if command == "report":
            return cmd_report(args)
    except (VersionError, CaptureError, report.Unpublishable) as e:
        print(f"止めました: {e}", file=sys.stderr)
        return 1
    raise SystemExit(f"知らないコマンド: {command}")
