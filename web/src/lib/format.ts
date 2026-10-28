/// 表示用のフォーマッタ（サイズ・日時）。ロケールは日本語固定（既存 UI に合わせる）。

const UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/// バイト数を人間可読に整形する（フォルダ等の null は "—"）。
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return "—";
  if (bytes < 1) return "0 B";
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), UNITS.length - 1);
  const value = bytes / 1024 ** exponent;
  const rounded = exponent === 0 ? value : Math.round(value * 10) / 10;
  return `${rounded} ${UNITS[exponent]}`;
}

const dateFormatter = new Intl.DateTimeFormat("ja-JP", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});

/// ISO 日時を「2026/06/25 10:00」風に整形する。
export function formatDateTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return dateFormatter.format(d);
}

/// 「3 分前」「2 日前」のような相対時刻（1 週間以上前は `absolute` の日時）。
export function relativeTime(
  iso: string | null | undefined,
  now: number,
  absolute: (iso: string) => string = formatDateTime,
): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "—";
  // 端末の時計のずれで先の時刻になったものは「たった今」に寄せる。
  const sec = Math.max(0, Math.round((now - t) / 1000));
  if (sec < 60) return "たった今";
  if (sec < 3600) return `${Math.floor(sec / 60)} 分前`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 時間前`;
  if (sec < 7 * 86400) return `${Math.floor(sec / 86400)} 日前`;
  return absolute(iso);
}
