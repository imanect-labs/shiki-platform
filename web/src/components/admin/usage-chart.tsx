"use client";

import * as React from "react";
import dynamic from "next/dynamic";

export type UsageMetric = "cost" | "tokens" | "calls";

type Day = { day: string; cost_usd_micros: number; tokens: number; calls: number };

const VALUE: Record<UsageMetric, (d: Day) => number> = {
  cost: (d) => d.cost_usd_micros,
  tokens: (d) => d.tokens,
  calls: (d) => d.calls,
};

const EMPTY_LABEL: Record<UsageMetric, string> = {
  cost: "この期間のコストは $0 です（単価未設定のモデルは $0 で集計されます）",
  tokens: "この期間の利用はありません",
  calls: "この期間の利用はありません",
};

/// recharts は重いので遅延読込する（読み込み中は同じ高さの枠を出してレイアウトを動かさない）。
const Bars = dynamic(() => import("./usage-chart-bars"), {
  ssr: false,
  loading: () => <div className="h-full animate-pulse rounded-md bg-muted/60" />,
});

/// 合計が 0 でない指標（コスト → トークン → 呼び出しの順）。グラフの初期表示に使う。
export function metricWithData(days: Day[]): UsageMetric {
  if (days.some((d) => d.cost_usd_micros > 0)) return "cost";
  if (days.some((d) => d.tokens > 0)) return "tokens";
  return "calls";
}

/// 日別の棒グラフ（単一系列・ホバーで日付と値）。0 埋め済みの全日付を受け取る。
export function UsageChart({
  days,
  metric,
  height = 200,
  label,
}: {
  days: Day[];
  metric: UsageMetric;
  height?: number;
  /// スクリーンリーダ向けの要約。
  label: string;
}) {
  const rows = days.map((d) => ({ day: d.day, value: VALUE[metric](d) }));
  if (rows.every((r) => r.value === 0)) {
    return (
      <div
        className="flex items-center justify-center rounded-md border border-dashed text-xs text-muted-foreground"
        style={{ height }}
      >
        {EMPTY_LABEL[metric]}
      </div>
    );
  }
  return (
    <div style={{ height }}>
      <Bars rows={rows} metric={metric} height={height} label={label} />
    </div>
  );
}
