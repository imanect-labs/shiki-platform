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
