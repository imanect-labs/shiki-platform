import type * as React from "react";

/// 四季(shiki)アクセントの共有ヘルパ。色そのものは globals.css の semantic token
/// （--season-spring/summer/autumn/winter）が正本で、ここは「巡回（春→夏→秋→冬）」の
/// 規則だけを一元化する。各コンポーネントは seasonVar(i) を CSS 変数として受け取り、
/// アイコン色やホバーのごく薄い差し色に使う（彩度は token 側で抑えてある）。
export const SEASONS = ["spring", "summer", "autumn", "winter"] as const;
export type Season = (typeof SEASONS)[number];

/// インデックスを 0..3 に丸めて対応する季節トークンの var() 文字列を返す。
/// 負値でも安全に巡回する。
export function seasonVar(i: number): string {
  const season = SEASONS[((i % SEASONS.length) + SEASONS.length) % SEASONS.length];
  return `var(--season-${season})`;
}

/// 現在の月から四季のインデックス（春0/夏1/秋2/冬3）を返す。差し色のブランド/区切りに使う。
export function currentSeasonIndex(): number {
  const m = new Date().getMonth() + 1;
  if (m >= 3 && m <= 5) return 0;
  if (m >= 6 && m <= 8) return 1;
  if (m >= 9 && m <= 11) return 2;
  return 3;
}

/// 季節アクセントを CSS 変数 `--season` として要素に注入する style を返す。
/// これで className 側は `hover:bg-[var(--season)]/[0.08]` `group-hover:text-[var(--season)]`
/// のように季節色を参照できる（shortcut-grid のホバー点灯レシピを全所で再利用するため）。
/// 例: <div style={seasonAccentStyle(i)} className="hover:text-[var(--season)]">
export function seasonAccentStyle(i: number): React.CSSProperties {
  return { ["--season" as string]: seasonVar(i) } as React.CSSProperties;
}
