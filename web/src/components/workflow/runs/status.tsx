"use client";

/// 実行履歴の状態語彙（run/step の日本語ラベル・バッジ・時刻/所要時間の整形）。
///
/// 文字列は backend の RunStatus/StepStatus/RunEventKind（vocab 単一定義）と対応する。
/// 未知の値はそのまま表示する（新語彙が増えても UI が壊れない fail-open 表示）。

import { Clock, Loader2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import type { ScheduledAtSource, WaitingFor } from "@/lib/workflow-run-api";

type BadgeVariant = "success" | "warning" | "destructive" | "muted" | "secondary";

const RUN_STATUS: Record<string, { label: string; variant: BadgeVariant; live?: boolean }> = {
  queued: { label: "順番待ち", variant: "muted" },
  running: { label: "実行中", variant: "secondary", live: true },
  succeeded: { label: "成功", variant: "success" },
  failed: { label: "失敗", variant: "destructive" },
  cancelled: { label: "中止", variant: "muted" },
};

/// 状態の絞り込みの選択肢。サーバーの絞り込みは run の状態だけを見るので、「実行中」を選ぶと待機中（承認・変更・時刻を
/// 待っている run）も出る。選択肢の名前でそれを言う（バッジは待機中を分けて出す・BUG-119）。
export const RUN_STATUS_OPTIONS = Object.entries(RUN_STATUS).map(([value, s]) => ({
  value,
  label: value === "running" ? "実行中・待機中" : s.label,
}));

export const TRIGGER_KIND_LABELS: Record<string, string> = {
  interactive: "手動",
  schedule: "スケジュール",
  event: "イベント",
};

export const TRIGGER_KIND_OPTIONS = Object.entries(TRIGGER_KIND_LABELS).map(
  ([value, label]) => ({ value, label }),
);

/// 予定の日時の見出し（実行の詳細とスマホの一覧・由来はサーバーが run の行から導く・GAP-89）。
export const SCHEDULED_AT_LABELS: Record<ScheduledAtSource, string> = {
  schedule: "予定の日時",
  specified: "指定した予定の日時",
  parent: "引き継いだ予定の日時",
};

/// PC の一覧の「予定の日時」の列で、日時に添える由来（スケジュールの予定の時刻には添えない）。
export const SCHEDULED_AT_MARKS: Record<ScheduledAtSource, string | null> = {
  schedule: null,
  specified: "指定した日時",
  parent: "引き継いだ日時",
};

/// 実行の詳細で、予定の日時の由来を言う文（スケジュールの予定の時刻は見出しで足りるので言わない）。`specified` は、
/// 実行のダイアログで指定した日時と、「同じ入力でもう一度実行」が元の実行から引き継いだ日時の両方（由来は run の行から
/// 導くので、2 つを分けない）。
export const SCHEDULED_AT_NOTES: Record<ScheduledAtSource, string | null> = {
  schedule: null,
  specified: "手動で実行するときに指定した日時か、やり直した実行の予定の日時を使いました。",
  parent: "「別のワークフローを実行」で始めた実行です。始めた実行の予定の日時を引き継いでいます。",
};

export function isTerminalRunStatus(status: string): boolean {
  return status === "succeeded" || status === "failed" || status === "cancelled";
}

/// 待っているものの呼び名（ブロックの状態の名前・付録 C の用語集）。run の待機中は「待機中（承認待ち）」と添える。
export const WAITING_LABELS: Record<WaitingFor, string> = {
  approval: "承認待ち",
  event: "変更待ち",
  timer: "時刻待ち",
  // ファイルの本文の取り込みを待って読み直す（ブロックだけ。実行は実行中のまま・D-125）。
  index: "取り込み待ち",
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
