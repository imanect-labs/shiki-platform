/// 発言の時刻の表示（日付の区切り・行の時刻）。端末のタイムゾーンで出す。

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/// 日付の区切りの見出し（今日・昨日・M月D日・年が違えば YYYY年M月D日）。
export function dayLabel(iso: string, now = new Date()): string {
  const d = new Date(iso);
  if (sameDay(d, now)) return "今日";
  const y = new Date(now);
  y.setDate(now.getDate() - 1);
  if (sameDay(d, y)) return "昨日";
  const md = `${d.getMonth() + 1}月${d.getDate()}日`;
  return d.getFullYear() === now.getFullYear() ? md : `${d.getFullYear()}年${md}`;
}

/// 行の時刻（HH:mm）。
export function timeLabel(iso: string): string {
  const d = new Date(iso);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/// 日付と時刻（検索結果・通知の行）。
export function dateTimeLabel(iso: string, now = new Date()): string {
  return `${dayLabel(iso, now)} ${timeLabel(iso)}`;
}
