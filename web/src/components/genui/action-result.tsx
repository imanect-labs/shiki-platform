"use client";

/// アクション実行結果の小さな表示部品（フォーム/ボタン共用）。

import { AlertCircle } from "lucide-react";

import type { UiActionResult } from "@/lib/artifact-api";

export function describeActionError(err: unknown): string {
  return err instanceof Error ? err.message : "アクションの実行に失敗しました";
}

/// 結果からユーザー向けの一言を組み立てる（束縛種別ごと）。
export function describeActionResult(res: UiActionResult): string {
  const r = res.result;
  if (r.kind === "workflow") {
    const runId = typeof r.run_id === "string" ? r.run_id : null;
    return runId ? `ワークフローを起動しました（run: ${runId.slice(0, 8)}…）` : "ワークフローを起動しました";
  }
  if (r.kind === "tool") {
    const content = typeof r.content === "string" ? r.content : "";
    return content.length > 200 ? `${content.slice(0, 200)}…` : content || "実行しました";
  }
  return "実行しました";
}

export function ActionResultNote({ error, note }: { error?: string | null; note?: string | null }) {
  if (error) {
    return (
      <p className="flex items-start gap-1.5 text-xs text-destructive" role="alert">
        <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
        {error}
      </p>
    );
  }
  if (note) {
    return <p className="whitespace-pre-wrap text-xs text-muted-foreground">{note}</p>;
  }
  return null;
}
