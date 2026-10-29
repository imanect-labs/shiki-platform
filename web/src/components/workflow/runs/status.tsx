"use client";

/// 実行履歴の状態語彙（run/step の日本語ラベル・バッジ・時刻/所要時間の整形）。
///
/// 文字列は backend の RunStatus/StepStatus/RunEventKind（vocab 単一定義）と対応する。
/// 未知の値はそのまま表示する（新語彙が増えても UI が壊れない fail-open 表示）。

import { Clock, Loader2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import type { WaitingFor } from "@/lib/workflow-run-api";

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

/// 待っているものの呼び名（ブロックの状態の名前・付録 C の用語集）。run の待機中は「待機中（承認待ち）」と添える。
export const WAITING_LABELS: Record<WaitingFor, string> = {
  approval: "承認待ち",
  event: "変更待ち",
  timer: "時刻待ち",
};

/// run の状態のバッジ。待機中（`running` で、承認・変更・時刻を待っている）は実行中と分け、回る印を出さない。
/// 待機中かどうかはサーバーが run の状態から導いた `waitingFor` で決める（画面で推し量らない・BUG-119）。
export function RunStatusBadge({ status, waitingFor = null }: { status: string; waitingFor?: WaitingFor | null }) {
  if (status === "running" && waitingFor) {
    return (
      <Badge variant="warning" data-testid="run-status-waiting" data-waiting-for={waitingFor}>
        <Clock className="size-3" aria-hidden />
        待機中（{WAITING_LABELS[waitingFor]}）
      </Badge>
    );
  }
  const s = RUN_STATUS[status] ?? { label: status, variant: "muted" as const };
  return (
    <Badge variant={s.variant}>
      {/* 状態は文字で示す（ドットは使わない）。実行中だけ回転アイコンで生きている感を出す。 */}
      {s.live ? <Loader2 className="size-3 animate-spin" aria-hidden /> : null}
      {s.label}
    </Badge>
  );
}

/// step の状態（timeline 用・run より語彙が細かい）。待っている step は、サーバーが返す待っているもの
/// （`waitingFor`）があればその呼び名（[`WAITING_LABELS`]）を先に使う。「待機中」は待っている run にだけ使う。
export const STEP_STATUS_LABELS: Record<string, string> = {
  pending: "前のブロック待ち",
  ready: "順番待ち",
  running: "実行中",
  waiting_timer: "時刻待ち",
  waiting_event: "変更待ち",
  waiting_map: "繰り返しの完了待ち",
  succeeded: "成功",
  failed: "失敗",
  skipped: "スキップ",
  cancelled: "中止",
};

/// run の fail_reason（engine 側の理由コード）の日本語説明。
export const FAIL_REASON_LABELS: Record<string, string> = {
  run_timeout: "実行全体の制限時間を超えました",
  // 承認を待つ間に委譲・テーブルの権限が外れた（engine の `approval::ACTOR_UNAVAILABLE`・D-62）。
  actor_unavailable: "承認を待つ間に、ワークフローを実行する権限（委譲またはテーブルの権限）が外れたため止めました",
  // 承認の期限が切れ、期限切れの扱いが「失敗にする」だった（engine の `approval::APPROVAL_TIMEOUT`・BUG-70）。
  approval_timeout: "承認の期限までに判断されなかったため止めました",
  // 変更を待つ期限が切れ、期限を過ぎたときの扱いが「実行を失敗にする」だった（engine の
  // `run::failure::WAIT_TIMEOUT`・BUG-112。手書きの対応物は BUG-113 で生成に替える）。
  wait_timeout: "待つ期限までに変更が起きなかったため、実行を止めました。",
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
