"use client";

/**
 * 自律エージェントの進捗 UI（Phase 5 / Task 5.11。計画は task-panel.tsx）:
 * - 承認ダイアログ（破壊系/egress/高コスト操作の human-in-the-loop）
 * - 予算警告バナー
 * チャットの content blocks レンダリングと同じ器に差し込む（既存 UI を再利用）。
 */

import type * as React from "react";

import { DataWriteSummary } from "./data-approval-summary";
import { DriveMoveSummary } from "./drive-move-approval-summary";
import type { ApprovalRequest } from "@/lib/chat-api";

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
  cardRef,
}: {
  request: ApprovalRequest;
  pending: boolean;
  onDecision: (approved: boolean) => void;
  /// 画面外にあるかの判定と「確認する」での移動先（承認待ちの帯・approval-notice.tsx）。
  cardRef?: React.Ref<HTMLDivElement>;
}) {
  return (
    <div
      ref={cardRef}
      role="group"
      tabIndex={-1}
      aria-label="承認が必要です"
      data-testid="approval-card"
      className="rounded-xl border border-primary/40 bg-primary/5 p-3 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <div className="mb-1 flex items-center gap-2">
        <span className="inline-flex size-5 items-center justify-center rounded-full bg-primary/15 text-primary">
          <svg viewBox="0 0 24 24" className="size-3.5" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M12 9v4m0 4h.01M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" />
          </svg>
        </span>
        <span className="text-[13px] font-semibold text-foreground">承認が必要です</span>
      </div>
      {request.name === "data_record_write" || request.name === "drive.move" ? (
        <>
          {/* 業務データの書込はテーブル名・対象・変える列を、ファイルの移動は何をどこへ移すかを
              読める形で（生の id と JSON は畳んでおく）。 */}
          {request.name === "drive.move" ? (
            <DriveMoveSummary input={request.input} />
          ) : (
            <DataWriteSummary input={request.input} />
          )}
          <details className="mb-2 text-[11px] text-muted-foreground">
            <summary className="cursor-pointer select-none">送る内容（JSON）</summary>
            <pre className="mt-1 max-h-24 overflow-auto overscroll-contain rounded-lg bg-secondary/60 p-2 text-foreground/70">
              {safeJson(request.input)}
            </pre>
          </details>
        </>
      ) : (
        <>
          <p className="mb-1 text-[13px] text-foreground/80 [overflow-wrap:anywhere]">
            ツール <code className="rounded bg-secondary px-1 py-0.5 text-[12px]">{request.name}</code> の実行に承認が必要です。
          </p>
          <p className="mb-2 text-[12px] text-muted-foreground">{request.reason}</p>
          <pre className="mb-2 max-h-24 overflow-auto overscroll-contain rounded-lg bg-secondary/60 p-2 text-[11px] text-foreground/70">
            {safeJson(request.input)}
          </pre>
        </>
      )}
      {/* スマホは親指で押しやすいよう幅いっぱいの 2 分割（誤タップしないよう間隔を空ける）。 */}
      <div className="flex items-center gap-2 max-md:gap-3">
        <button
          type="button"
          disabled={pending}
          onClick={() => onDecision(true)}
          className="inline-flex h-8 items-center justify-center rounded-full bg-primary px-3 text-[13px] font-medium text-primary-foreground disabled:opacity-50 max-md:h-11 max-md:flex-1 max-md:text-sm"
        >
          承認して続行
        </button>
        <button
          type="button"
          disabled={pending}
          onClick={() => onDecision(false)}
          className="inline-flex h-8 items-center justify-center rounded-full border border-border px-3 text-[13px] font-medium text-foreground/80 hover:bg-secondary disabled:opacity-50 max-md:h-11 max-md:flex-1 max-md:text-sm"
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
