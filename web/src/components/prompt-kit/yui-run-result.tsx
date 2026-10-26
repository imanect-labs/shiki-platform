// 図のボタンで起動したワークフローの記録と、ワークフローが画面に返した結果（workflow.reply・GAP-09・D-28）。
//
// 結果は押した本人の記録にだけ付く（サーバーが関門を持つ）。結果の中身は外から来た文章として描く:
// - Markdown の中の操作を持つ図・表のフェンスはサーバーが ```text に書き換え済み。画面側もボタン・表の
//   宛先を null にした文脈で描き、画像は読み込まずにリンクにし、外部リンクにはホスト名を添える。
// - 結果から別の起動はできない（次の操作は作成者が画面に置いたボタンで受ける）。
"use client";

import * as React from "react";
import Link from "next/link";
import {
  CheckCircle2,
  ChevronDown,
  CircleDashed,
  Clock,
  FileText,
  Hourglass,
  Loader2,
  MinusCircle,
  XCircle,
} from "lucide-react";

import { DataGridHostContext } from "@/components/data/data-grid-host";
import type { components } from "@/generated/api";
import { cn } from "@/lib/utils";
import { Markdown } from "./markdown";
import { YuiActionContext, type YuiRun } from "./yui-actions";

type ReplySection = components["schemas"]["ReplySection"];
type ReplyTone = components["schemas"]["ReplyTone"];

type StatusView = { label: string; icon: React.ElementType; tone: string; spin?: boolean };

const STATUS: Record<YuiRun["status"], StatusView> = {
  starting: { label: "起動中", icon: Loader2, tone: "text-muted-foreground", spin: true },
  queued: { label: "順番待ち", icon: CircleDashed, tone: "text-muted-foreground" },
  running: { label: "実行中", icon: Loader2, tone: "text-muted-foreground", spin: true },
  waiting: { label: "待機中", icon: Clock, tone: "text-[color:var(--season-autumn)]" },
  succeeded: { label: "成功", icon: CheckCircle2, tone: "text-primary" },
  failed: { label: "失敗", icon: XCircle, tone: "text-destructive" },
  cancelled: { label: "取り消し", icon: MinusCircle, tone: "text-muted-foreground" },
  skipped: { label: "見送り（同時実行の上限）", icon: MinusCircle, tone: "text-muted-foreground" },
};

const TONE_BORDER: Record<ReplyTone, string> = {
  neutral: "border-border",
  success: "border-[color:var(--season-summer)]",
  warning: "border-[color:var(--season-autumn)]",
  error: "border-destructive",
};

/// 状態の表示。待機中は何を待っているか、成功でも業務上の失敗・注意を返したなら「完了」とその色にする
/// （「成功」と「貸し出しできませんでした」を並べない）。
function statusView(r: YuiRun): StatusView {
  if (r.status === "waiting" && r.waiting_for === "approval") {
    return { label: "承認待ち", icon: Hourglass, tone: STATUS.waiting.tone };
  }
  if (r.status === "succeeded") {
    const tones = (r.reply?.sections ?? []).map((s) => s.tone);
    if (tones.includes("error")) return { label: "完了", icon: CheckCircle2, tone: "text-destructive" };
    if (tones.includes("warning")) {
      return { label: "完了", icon: CheckCircle2, tone: "text-[color:var(--season-autumn)]" };
    }
  }
  return STATUS[r.status];
}

function time(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleString("ja-JP", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function isOpen(r: YuiRun): boolean {
  return r.status === "starting" || r.status === "queued" || r.status === "running";
}

/// 結果の欄に出すものがあるか（無ければ開閉のボタンを出さない）。
function hasResultArea(r: YuiRun): boolean {
  return (
    isOpen(r) ||
    r.status === "waiting" ||
    r.failure != null ||
    r.reply != null ||
    Boolean(r.has_reply) ||
    Boolean(r.result_expired)
  );
}

/// 図のボタンで起動したワークフローの記録（図の下・新しいものが下）。新しい 1 件は結果を開いた状態で出す。
export function YuiRunList({ runs }: { runs: readonly YuiRun[] }) {
  if (runs.length === 0) return null;
  const latest = runs[runs.length - 1];
  return (
    <>
      <ul className="mt-2 min-w-0 space-y-1 text-[12px]" aria-label="起動したワークフロー" data-testid="yui-runs">
        {runs.map((r, i) => (
          <YuiRunRow key={`${r.action_id}-${r.invoked_at}-${r.run_id ?? ""}`} run={r} latest={i === runs.length - 1} />
        ))}
      </ul>
      {/* 最新の起動の状態の変化を読み上げる（結果の中身は読み上げない）。 */}
      <p className="sr-only" aria-live="polite">
        {latest ? `「${latest.label}」は${statusView(latest).label}` : ""}
      </p>
    </>
  );
}

function YuiRunRow({ run: r, latest }: { run: YuiRun; latest: boolean }) {
  const s = statusView(r);
  const Icon = s.icon;
  const values = (r.values ?? []).map((v) => `${v.label}: ${v.value}`).join("・");
  const resultId = React.useId();
  const area = hasResultArea(r);
  // 新しい 1 件は開き、古いものは閉じておく（押すたびに一覧が伸びて画面を押し下げない）。
  const [open, setOpen] = React.useState(latest);
  const wasLatest = React.useRef(latest);
  React.useEffect(() => {
    // 新しい起動が足されて最新でなくなったら閉じる。
    if (wasLatest.current && !latest) setOpen(false);
    wasLatest.current = latest;
  }, [latest]);
  return (
    <li
      className="min-w-0 rounded-lg border border-border bg-secondary/30 px-2.5 py-1.5"
      data-status={r.status}
      data-run-id={r.run_id ?? undefined}
    >
      <div className="flex min-w-0 items-start gap-2">
        <Icon className={cn("mt-0.5 size-3.5 shrink-0", s.tone, s.spin && "animate-spin")} aria-hidden />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline gap-x-2">
            <span className="font-medium text-foreground">
              {r.label} → {r.workflow_name}
            </span>
            <span className={s.tone}>{s.label}</span>
            <span className="text-muted-foreground">{time(r.invoked_at)}</span>
          </div>
          {values ? (
            <p className="truncate text-muted-foreground" title={values}>
              {values}
            </p>
          ) : null}
        </div>
        {area ? (
          <button
            type="button"
            className="-my-0.5 -mr-1 flex shrink-0 items-center gap-0.5 rounded-md px-1.5 py-0.5 text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:py-1.5"
            aria-expanded={open}
            aria-controls={resultId}
            onClick={() => setOpen((v) => !v)}
          >
            結果
            <ChevronDown className={cn("size-3.5 transition-transform", open && "rotate-180")} aria-hidden />
          </button>
        ) : null}
      </div>
      {area && open ? (
        <div id={resultId} className="mt-1.5 min-w-0 border-t border-border/60 pt-1.5" data-testid="yui-run-result">
          <YuiRunResult run={r} />
        </div>
      ) : null}
    </li>
  );
}

/// 1 件の結果（待ちの状態・失敗の理由・返した節）。結果が付いていない古い記録は開いたときに読む。
export function YuiRunResult({ run }: { run: YuiRun }) {
  const host = React.useContext(YuiActionContext);
  // 読み込んだ結果（どの run のものか）。
  const [fetched, setFetched] = React.useState<{ runId: string; run: YuiRun } | null>(null);
  const [loadError, setLoadError] = React.useState(false);
  const needsLoad = run.reply == null && Boolean(run.has_reply) && run.run_id != null;
  // 読み込みの関数は依存に入れない（画面は記録を読み直すたびに作り直すので、入れると開いている古い行の結果を
  // 読み直しのたびに取り直す）。同じ run を読み込み済みなら取り直さない。
  const loadRun = React.useRef(host?.loadRun);
  loadRun.current = host?.loadRun;
  const loaded = fetched && fetched.runId === run.run_id ? fetched.run : null;
  const fetchedRunId = fetched?.runId ?? null;
  React.useEffect(() => {
    const load = loadRun.current;
    const runId = run.run_id;
    if (!needsLoad || !load || !runId || fetchedRunId === runId) return;
    let live = true;
    setLoadError(false);
    load(runId).then(
      (r) => live && setFetched({ runId, run: r }),
      () => live && setLoadError(true),
    );
    return () => {
      live = false;
    };
  }, [needsLoad, run.run_id, fetchedRunId]);
  const r = needsLoad && loaded ? { ...run, reply: loaded.reply, failure: loaded.failure ?? run.failure } : run;
  const sections = r.reply?.sections ?? [];
  const omitted = r.reply?.omitted ?? 0;
  return (
    <div className="min-w-0 space-y-1.5 text-[13px]">
      {isOpen(r) ? (
        <p className="flex items-center gap-1.5 text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
          実行中…（結果はここに出ます）
        </p>
      ) : null}
      {r.status === "waiting" ? (
        <p className="flex items-center gap-1.5 text-muted-foreground">
          {r.waiting_for === "approval" ? (
            <>
              <Hourglass className="size-3.5" aria-hidden />
              承認を待っています（担当者の受信箱に出ています）
            </>
          ) : (
            <>
              <Clock className="size-3.5" aria-hidden />
              {r.waiting_for === "timer" ? "決めた時間まで待っています" : "できごと（ファイルやデータの変更）を待っています"}
            </>
          )}
        </p>
      ) : null}
      {r.failure ? (
        <div
          className="rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-destructive"
          role="alert"
          data-testid="yui-run-failure"
        >
          <p>{r.failure.message}</p>
          <p className="mt-0.5 text-[11px] text-destructive/80">参照番号: {r.failure.reference}</p>
        </div>
      ) : null}
      {needsLoad && !loaded ? (
        loadError ? (
          <p className="text-muted-foreground">結果を読み込めませんでした。ページを読み直してください</p>
        ) : (
          <p className="flex items-center gap-1.5 text-muted-foreground" role="status">
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
            結果を読み込み中…
          </p>
        )
      ) : null}
      {sections.map((s, i) => (
        <ReplySectionView key={i} section={s} failedAfter={r.status === "failed"} />
      ))}
      {omitted > 0 ? <p className="text-[11px] text-muted-foreground">ほか {omitted} 件の結果は省きました</p> : null}
      {r.result_expired ? (
        <p className="text-[11px] text-muted-foreground">実行履歴の保持期間を過ぎたため、結果は表示できません</p>
      ) : null}
    </div>
  );
}

/// 結果の 1 節（見出し・色合い・形ごとの中身）。
function ReplySectionView({ section: s, failedAfter }: { section: ReplySection; failedAfter: boolean }) {
  return (
    <section
      className={cn("min-w-0 rounded-md border-l-2 bg-background/60 py-1 pl-2.5 pr-1", TONE_BORDER[s.tone])}
      data-testid="yui-run-section"
      data-format={s.format}
      data-tone={s.tone}
    >
      {s.label ? <h4 className="mb-0.5 text-[12px] font-semibold text-foreground">{s.label}</h4> : null}
      <SectionBody section={s} />
      {s.truncated ? <p className="mt-0.5 text-[11px] text-muted-foreground">長いため途中までを表示しています</p> : null}
      {failedAfter && s.tone === "success" ? (
        <p className="mt-0.5 text-[11px] text-destructive">この後の処理で失敗しました</p>
      ) : null}
    </section>
  );
}

function SectionBody({ section: s }: { section: ReplySection }) {
  switch (s.format) {
    case "text":
      return <p className="whitespace-pre-wrap text-foreground [overflow-wrap:anywhere]">{s.text}</p>;
    case "markdown":
      // 操作を持つ図・表は無効の文脈で描く（サーバーも ```text に書き換え済み）。
      return (
        <YuiActionContext.Provider value={null}>
          <DataGridHostContext.Provider value={null}>
            <Markdown untrusted className="text-[13px]">
              {s.text}
            </Markdown>
          </DataGridHostContext.Provider>
        </YuiActionContext.Provider>
      );
    case "table":
      return (
        <>
          <div className="max-w-full overflow-x-auto overscroll-x-contain rounded-md border border-border">
            <table className="w-full border-collapse text-[12px]">
              <thead className="bg-muted/50">
                <tr>
                  {s.columns.map((c, i) => (
                    <th key={i} className="whitespace-nowrap border-b border-border px-2 py-1.5 text-left font-semibold">
                      {c}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {s.rows.map((row, i) => (
                  <tr key={i}>
                    {row.map((cell, j) => (
                      <td key={j} className="min-w-[5rem] border-b border-border/60 px-2 py-1.5 [overflow-wrap:break-word]">
                        {cell}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {s.total_rows > s.rows.length ? (
            <p className="mt-0.5 text-[11px] text-muted-foreground">ほか {s.total_rows - s.rows.length} 行</p>
          ) : null}
        </>
      );
    case "file_link":
      return s.files.length === 0 ? (
        <p className="text-muted-foreground">ファイルはありません</p>
      ) : (
        <ul className="space-y-0.5">
          {s.files.map((f) => (
            <li key={f.id} className="min-w-0">
              {/* 開くときに通常の権限で確かめる（見えないファイルは開けない）。 */}
              <Link
                href={`/file/${encodeURIComponent(f.id)}`}
                className="inline-flex min-w-0 max-w-full items-center gap-1 font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary"
              >
                <FileText className="size-3.5 shrink-0" aria-hidden />
                <span className="truncate">{f.name}</span>
              </Link>
            </li>
          ))}
        </ul>
      );
    default:
      return null;
  }
}
