/// generative UI チャートの共有トークン・ヘルパ（配色は四季アクセント CSS 変数から取り、
/// ライト/ダーク両対応。生 hex は使わない）。

import type { ChartSpec } from "@/generated/gui-spec";

/// カテゴリカルパレット（テーマ CSS 変数・ライト/ダークで自動追従）。
export const PALETTE = [
  "var(--season-winter)",
  "var(--season-autumn)",
  "var(--season-summer)",
  "var(--season-spring)",
  "var(--primary)",
] as const;

/// 系列インデックス → 一貫した色。
export function colorFor(i: number): string {
  return PALETTE[((i % PALETTE.length) + PALETTE.length) % PALETTE.length];
}

export const AXIS_TICK = { fontSize: 11, fill: "var(--muted-foreground)" } as const;

export const TOOLTIP_STYLE = {
  backgroundColor: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 10,
  fontSize: 12,
  color: "var(--foreground)",
  boxShadow: "var(--shadow-md, 0 4px 12px rgb(0 0 0 / 0.08))",
} as const;

/// 単一系列名（series 省略時の凡例ラベル）。
export const DEFAULT_SERIES = "値";

export type Row = Record<string, string | number>;

/// points（x/y/series）→ recharts の行形式（x をキーに系列を列へ）。系列の出現順を保つ。
export function toRows(spec: ChartSpec): { rows: Row[]; series: string[] } {
  const series: string[] = [];
  const byX = new Map<string, Row>();
  for (const p of spec.data ?? []) {
    const name = p.series ?? DEFAULT_SERIES;
    if (!series.includes(name)) series.push(name);
    const row = byX.get(p.x) ?? { x: p.x };
    row[name] = p.y;
    byX.set(p.x, row);
  }
  return { rows: [...byX.values()], series };
}

/// x ラベルごとに全系列を合算した {name, value}（pie/donut/funnel/radial_bar/treemap 用）。
export function toTotals(spec: ChartSpec): { name: string; value: number }[] {
  const byX = new Map<string, number>();
  const order: string[] = [];
  for (const p of spec.data ?? []) {
    if (!byX.has(p.x)) order.push(p.x);
    byX.set(p.x, (byX.get(p.x) ?? 0) + (Number.isFinite(p.y) ? p.y : 0));
  }
  return order.map((name) => ({ name, value: byX.get(name) ?? 0 }));
}
