"use client";

/**
 * 自律エージェントの進捗 UI（Phase 5 / Task 5.11）:
 * - 計画（サブタスク）パネル
 * - 承認ダイアログ（破壊系/egress/高コスト操作の human-in-the-loop）
 * - 予算警告バナー
 * チャットの content blocks レンダリングと同じ器に差し込む（既存 UI を再利用）。
 */

import { cn } from "@/lib/utils";
import type { ApprovalRequest, PlanSubtask } from "@/lib/chat-api";

const STATUS_META: Record<string, { label: string; dot: string; text: string }> = {
  todo: { label: "未着手", dot: "bg-muted-foreground/40", text: "text-foreground/70" },
  doing: { label: "進行中", dot: "bg-primary animate-pulse", text: "text-foreground" },
  done: { label: "完了", dot: "bg-emerald-500", text: "text-foreground/50 line-through" },
  blocked: { label: "保留", dot: "bg-amber-500", text: "text-amber-600 dark:text-amber-400" },
};

/** 計画（サブタスク列）のチェックリスト。 */
export function PlanPanel({ subtasks }: { subtasks: PlanSubtask[] }) {
  if (subtasks.length === 0) return null;
  const done = subtasks.filter((s) => s.status === "done").length;
  return (
    <div className="rounded-xl border border-border bg-card/60 p-3">
      <div className="mb-2 flex items-center justify-between">
        <span className="text-[13px] font-semibold text-foreground/80">計画</span>
        <span className="text-[12px] tabular-nums text-muted-foreground">
          {done}/{subtasks.length}
        </span>
      </div>
      <ol className="space-y-1.5">
        {subtasks.map((s) => {
          const meta = STATUS_META[s.status] ?? STATUS_META.todo;
          return (
            <li key={s.id} className="flex items-start gap-2 text-[13px]">
              <span className={cn("mt-1.5 size-2 shrink-0 rounded-full", meta.dot)} aria-hidden />
              <span className={meta.text}>{s.title}</span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/** 予算警告バナー（上限接近）。 */
export function BudgetBanner({ kind, used, limit }: { kind: string; used: number; limit: number }) {
  const label =
    { steps: "ステップ", time: "時間", tokens: "トークン", cost: "コスト" }[kind] ?? kind;
  const pct = limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 0;
  return (
    <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[12px] text-amber-700 dark:text-amber-300">
      予算警告: {label}が上限に接近しています（{used.toLocaleString()} / {limit.toLocaleString()}・{pct}%）
    </div>
  );
}

/** 承認ダイアログ（破壊系操作の確認・block しているエージェントを解く）。 */
export function ApprovalCard({
  request,
  pending,
  onDecision,
}: {
  request: ApprovalRequest;
  pending: boolean;
  onDecision: (approved: boolean) => void;
}) {
  return (
    <div className="rounded-xl border border-primary/40 bg-primary/5 p-3">
      <div className="mb-1 flex items-center gap-2">
        <span className="inline-flex size-5 items-center justify-center rounded-full bg-primary/15 text-primary">
          <svg viewBox="0 0 24 24" className="size-3.5" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M12 9v4m0 4h.01M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" />
          </svg>
        </span>
        <span className="text-[13px] font-semibold text-foreground">承認が必要です</span>
      </div>
      <p className="mb-1 text-[13px] text-foreground/80">
        ツール <code className="rounded bg-secondary px-1 py-0.5 text-[12px]">{request.name}</code> の実行に承認が必要です。
      </p>
      <p className="mb-2 text-[12px] text-muted-foreground">{request.reason}</p>
      <pre className="mb-2 max-h-24 overflow-auto rounded-lg bg-secondary/60 p-2 text-[11px] text-foreground/70">
        {safeJson(request.input)}
      </pre>
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={pending}
          onClick={() => onDecision(true)}
          className="inline-flex h-8 items-center rounded-full bg-primary px-3 text-[13px] font-medium text-primary-foreground disabled:opacity-50"
        >
          承認して続行
        </button>
        <button
          type="button"
          disabled={pending}
          onClick={() => onDecision(false)}
          className="inline-flex h-8 items-center rounded-full border border-border px-3 text-[13px] font-medium text-foreground/80 hover:bg-secondary disabled:opacity-50"
        >
          却下
        </button>
      </div>
    </div>
  );
}

function safeJson(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}
