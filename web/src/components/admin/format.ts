/// 管理コンソールの数値表記（会計は USD マイクロ・円は参考換算）。

const usdFmt = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});
const jpyFmt = new Intl.NumberFormat("ja-JP", { maximumFractionDigits: 0 });
const compactFmt = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
});
const intFmt = new Intl.NumberFormat("ja-JP");

/// マイクロ USD → `$1,234.56`。1 セント未満（0 を除く）は `<$0.01`。
export function formatUsd(micros: number): string {
  if (micros > 0 && micros < 10_000) return "<$0.01";
  return usdFmt.format(micros / 1_000_000);
}

/// マイクロ USD → 円の参考額（`約 ¥18,600`）。レート未設定は null。
export function formatJpy(micros: number, jpyPerUsd: number | null | undefined): string | null {
  if (!jpyPerUsd) return null;
  return `約 ¥${jpyFmt.format((micros / 1_000_000) * jpyPerUsd)}`;
}

/// トークン数など大きな数 → `182M`。
export function formatCompact(n: number): string {
  return compactFmt.format(n);
}

/// 件数 → `41,203`。
export function formatInt(n: number): string {
  return intFmt.format(n);
}

/// 単価（マイクロ USD / 100 万トークン）→ `$3.00`。0 は「—」。
export function formatPrice(microsPerMtok: number): string {
  return microsPerMtok > 0 ? usdFmt.format(microsPerMtok / 1_000_000) : "—";
}

/// `YYYY-MM-DD` → `10/9（金）`。
export function formatDay(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  const date = new Date(y, m - 1, d);
  const wd = "日月火水木金土"[date.getDay()];
  return `${m}/${d}（${wd}）`;
}

/// 日本時間の今日（`YYYY-MM-DD`）。
export function todayJst(): string {
  const now = new Date(Date.now() + 9 * 3600_000);
  return now.toISOString().slice(0, 10);
}

/// `YYYY-MM-DD` に日数を足す。
export function addDays(iso: string, days: number): string {
  const [y, m, d] = iso.split("-").map(Number);
  const date = new Date(Date.UTC(y, m - 1, d + days));
  return date.toISOString().slice(0, 10);
}

export const EFFORT_LABEL = { low: "速い", medium: "標準", high: "じっくり" } as const;
