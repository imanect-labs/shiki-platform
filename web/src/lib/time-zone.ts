/// 時間帯の言い方と、時間帯の上の日時の書き方（画面の文言の付録 C の P11）。

/// 時間帯の呼び名（日本の時刻は「日本時間」。ほかは名前のまま）。
export function zoneLabel(tz: string): string {
  if (tz === "Asia/Tokyo") return "日本時間";
  return tz;
}

/// その時間帯で見た日時を、日時の入力欄（`datetime-local`）の値の形 `YYYY-MM-DDTHH:mm` で返す。
export function localDateTimeIn(tz: string, at: Date = new Date()): string {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: tz,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(at);
  const get = (t: Intl.DateTimeFormatPartTypes) => parts.find((p) => p.type === t)?.value ?? "";
  return `${get("year")}-${get("month")}-${get("day")}T${get("hour")}:${get("minute")}`;
}

/// 日時の入力欄の値（`YYYY-MM-DDTHH:mm`）を、画面の文の日時の形（2026/11/09 09:00・付録 C の P11）にする。読めない値は
/// そのまま返す。
export function formatLocalDateTime(value: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(value);
  return m ? `${m[1]}/${m[2]}/${m[3]} ${m[4]}:${m[5]}` : value;
}
