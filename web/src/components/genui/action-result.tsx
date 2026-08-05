"use client";

/// アクション実行結果の小さな表示部品（フォーム/ボタン共用）。

import { AlertCircle, Loader2 } from "lucide-react";

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

/// 生成中に受け付けた操作の「順番待ち」表示。
///
/// カードは AI が本文を書き終える前に出るため、押しても即時には実行できない時間帯がある。
/// 以前はここを**押してからエラーで知らせて**いたが、生成が終わるとカードは確定メッセージ側で
/// 作り直されるため、それまでに入れた回答が丸ごと消えていた。いまは押した時点で受理し、
/// 生成が終わった瞬間に自動で送る。待っていることだけを控えめに伝える。
export function QueuedActionNote({ ready, submitted }: { ready: boolean; submitted: boolean }) {
  if (ready || !submitted) return null;
  return (
    <p className="flex items-center gap-1.5 text-xs text-muted-foreground" data-testid="genui-queued">
      <Loader2 className="size-3.5 shrink-0 animate-spin" aria-hidden />
      AI が書き終えたら送信します
    </p>
  );
}
