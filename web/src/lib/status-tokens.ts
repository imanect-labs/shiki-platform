/// ステータス色の単一定義（生 oklch リテラルの撲滅）。
///
/// ワークフローの run/step、CSV の保存状態、ノートの同期状態など「状態を色で示す」箇所は
/// すべてここの `StatusTone` に集約し、色は必ずセマンティック/四季トークンへ解決する。
/// これにより配色がライト/ダークで自動反転し、ブランドの四季アクセントと一貫する。
///
/// ※ 純粋な写像のみ（React 非依存・自己完結）。色は CSS 変数文字列で返し、呼び出し側が
///   `style={{ color: statusVar(status) }}` などで使う。

export type StatusTone = "success" | "danger" | "waiting" | "running" | "muted";

/// トーン → 前景色トークン（CSS 変数文字列）。
/// success=夏の新緑 / danger=破壊的 / waiting=冬の空 / running=プライマリ Navy / muted=控えめ。
export const TONE_VAR: Record<StatusTone, string> = {
  success: "var(--season-summer)",
  danger: "var(--destructive)",
  waiting: "var(--season-winter)",
  running: "var(--primary)",
  muted: "var(--muted-foreground)",
};

/// run/step のステータス文字列 → トーン。未知値は muted（fail-open 表示）。
const STATUS_TONE: Record<string, StatusTone> = {
  // run
  queued: "muted",
  running: "running",
  succeeded: "success",
  failed: "danger",
  cancelled: "muted",
  // step（run より細かい語彙）
  pending: "muted",
  ready: "muted",
  waiting_timer: "waiting",
  waiting_event: "waiting",
  waiting_map: "waiting",
  skipped: "muted",
};

export function statusTone(status: string): StatusTone {
  return STATUS_TONE[status] ?? "muted";
}

/// ステータス文字列 → 前景色トークン（近道）。
export function statusVar(status: string): string {
  return TONE_VAR[statusTone(status)];
}

/// 「実行中」系＝控えめな pulse を出すべき状態か（live インジケータ用）。
export function isLiveStatus(status: string): boolean {
  return (
    status === "running" ||
    status === "waiting_timer" ||
    status === "waiting_event" ||
    status === "waiting_map"
  );
}
