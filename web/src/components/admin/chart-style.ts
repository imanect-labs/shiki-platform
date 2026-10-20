/// 管理画面のグラフ（recharts）で共通の軸・ツールチップのスタイル（テーマの CSS 変数を使い、
/// ライト/ダークに追従する）。
export const AXIS_TICK = { fontSize: 11, fill: "var(--muted-foreground)" } as const;

export const TOOLTIP_STYLE = {
  backgroundColor: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 10,
  fontSize: 12,
  color: "var(--foreground)",
  boxShadow: "var(--shadow-md, 0 4px 12px rgb(0 0 0 / 0.08))",
} as const;
