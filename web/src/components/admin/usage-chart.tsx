"use client";

import * as React from "react";

import { YuiChart, yuiNumber } from "./yui-chart";

export type UsageMetric = "cost" | "tokens" | "calls";

type Day = { day: string; cost_usd_micros: number; tokens: number; calls: number };

/// 指標ごとの値（記法に書く素の数値）と単位。
const METRIC: Record<UsageMetric, { name: string; unit: string; value: (d: Day) => string }> = {
  cost: { name: "コスト", unit: "ドル", value: (d) => yuiNumber(d.cost_usd_micros / 1_000_000, 4) },
  tokens: { name: "トークン", unit: "トークン", value: (d) => yuiNumber(d.tokens) },
  calls: { name: "呼び出し", unit: "回", value: (d) => yuiNumber(d.calls) },
};

const EMPTY_LABEL: Record<UsageMetric, string> = {
  cost: "この期間のコストは $0 です（単価未設定のモデルは $0 で集計されます）",
  tokens: "この期間の利用はありません",
  calls: "この期間の利用はありません",
};

/// 合計が 0 でない指標（コスト → トークン → 呼び出しの順）。グラフの初期表示に使う。
export function metricWithData(days: Day[]): UsageMetric {
  if (days.some((d) => d.cost_usd_micros > 0)) return "cost";
  if (days.some((d) => d.tokens > 0)) return "tokens";
  return "calls";
}

/// 日別の推移（単一系列）。0 埋め済みの全日付を受け取る。チャットの本文の図と同じ Yui で描く（#783）。
/// 2 週間までは棒、それより長い期間は折れ線（Yui は 15 本以上の棒を横向きにするため）。
export function UsageChart({ days, metric }: { days: Day[]; metric: UsageMetric }) {
  const m = METRIC[metric];
  const source = React.useMemo(() => {
    const kind = days.length <= 14 ? "bar" : "line";
    const label = (iso: string) => {
      const [, mm, dd] = iso.split("-").map(Number);
      return `${mm}/${dd}`;
    };
    return [kind, `unit: ${m.unit}`, `日 | ${m.name}`, ...days.map((d) => `${label(d.day)} | ${m.value(d)}`)].join(
      "\n",
    );
  }, [days, m]);

  const empty = days.every((d) => Number(m.value(d)) === 0);
  if (empty) {
    return (
      <div className="flex h-[200px] items-center justify-center rounded-md border border-dashed text-xs text-muted-foreground">
        {EMPTY_LABEL[metric]}
      </div>
    );
  }
  return <YuiChart source={source} />;
}
