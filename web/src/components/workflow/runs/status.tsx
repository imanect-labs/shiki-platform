"use client";

/// 実行履歴の状態語彙（run/step の日本語ラベル・バッジ・時刻/所要時間の整形）。
///
/// 文字列は backend の RunStatus/StepStatus/RunEventKind（vocab 単一定義）と対応する。
/// 未知の値はそのまま表示する（新語彙が増えても UI が壊れない fail-open 表示）。

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

type BadgeVariant = "success" | "warning" | "destructive" | "muted" | "secondary";

const RUN_STATUS: Record<string, { label: string; variant: BadgeVariant; live?: boolean }> = {
  queued: { label: "順番待ち", variant: "muted" },
  running: { label: "実行中", variant: "secondary", live: true },
  succeeded: { label: "成功", variant: "success" },
  failed: { label: "失敗", variant: "destructive" },
  cancelled: { label: "中止", variant: "muted" },
};

export const RUN_STATUS_OPTIONS = Object.entries(RUN_STATUS).map(([value, s]) => ({
  value,
  label: s.label,
}));

export const TRIGGER_KIND_LABELS: Record<string, string> = {
  interactive: "手動",
  schedule: "スケジュール",
  event: "イベント",
};

export const TRIGGER_KIND_OPTIONS = Object.entries(TRIGGER_KIND_LABELS).map(
  ([value, label]) => ({ value, label }),
);

export function isTerminalRunStatus(status: string): boolean {
  return status === "succeeded" || status === "failed" || status === "cancelled";
}

export function RunStatusBadge({ status }: { status: string }) {
  const s = RUN_STATUS[status] ?? { label: status, variant: "muted" as const };
  return (
    <Badge variant={s.variant}>
      <span
        className={cn(
          "size-1.5 rounded-full bg-current",
          s.live && "animate-pulse",
        )}
        aria-hidden
      />
      {s.label}
    </Badge>
  );
}

/// step の状態（timeline 用・run より語彙が細かい）。
export const STEP_STATUS_LABELS: Record<string, string> = {
  pending: "待機中",
  ready: "順番待ち",
  running: "実行中",
  waiting_timer: "時間まで待機",
  waiting_event: "できごと待ち",
  waiting_map: "繰り返しの完了待ち",
  succeeded: "成功",
  failed: "失敗",
  skipped: "スキップ",
  cancelled: "中止",
};

/// run の fail_reason（engine 側の理由コード）の日本語説明。
export const FAIL_REASON_LABELS: Record<string, string> = {
  run_timeout: "実行全体の制限時間を超えました",
  step_failed: "途中のブロックが失敗しました",
  cancelled: "中止されました",
};

// ── 整形ヘルパ ──────────────────────────────────────────────────────

export function formatDateTime(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("ja-JP", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/// 所要時間（開始〜終了。実行中は現在時刻まで）。
export function formatDuration(
  startedAt: string | null,
  finishedAt: string | null,
): string {
  if (!startedAt) return "—";
  const start = new Date(startedAt).getTime();
  const end = finishedAt ? new Date(finishedAt).getTime() : Date.now();
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return "—";
  const sec = Math.round((end - start) / 1000);
  if (sec < 60) return `${sec}秒`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}分${sec % 60}秒`;
  return `${Math.floor(min / 60)}時間${min % 60}分`;
}
