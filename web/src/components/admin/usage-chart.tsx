"use client";

import * as React from "react";

import { YuiChart, yuiNumber } from "./yui-chart";

export type UsageMetric = "cost" | "tokens" | "calls";

type Day = { day: string; cost_usd_micros: number; tokens: number; calls: number };

type MetricDef = { name: string; unit: string; raw: (d: Day) => number; value: (d: Day) => string };

/// 有効数字 3 桁（1 セント未満の日も 0 に丸めない）。
const sig3 = (n: number) => String(Number(n.toPrecision(3)));

/// 指標ごとの値（記法に書く素の数値）と単位。コストは日ごとの最大が 1 ドル未満ならセントで書く
/// （小さなドルの値では目盛りの丸めで同じ数字が並ぶため）。
function metricDef(metric: UsageMetric, days: Day[]): MetricDef {
  if (metric === "tokens")
    return { name: "トークン", unit: "トークン", raw: (d) => d.tokens, value: (d) => yuiNumber(d.tokens) };
  if (metric === "calls")
    return { name: "呼び出し", unit: "回", raw: (d) => d.calls, value: (d) => yuiNumber(d.calls) };
  const raw = (d: Day) => d.cost_usd_micros;
  const cents = Math.max(0, ...days.map(raw)) < 1_000_000;
  return cents
    ? { name: "コスト", unit: "セント", raw, value: (d) => sig3(d.cost_usd_micros / 10_000) }
    : { name: "コスト", unit: "ドル", raw, value: (d) => yuiNumber(d.cost_usd_micros / 1_000_000, 2) };
}

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
  const m = React.useMemo(() => metricDef(metric, days), [metric, days]);
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

  const empty = days.every((d) => m.raw(d) === 0);
  if (empty) {
    return (
      <div className="flex h-[200px] items-center justify-center rounded-md border border-dashed text-xs text-muted-foreground">
        {EMPTY_LABEL[metric]}
      </div>
    );
  }
  return (
    <YuiChart
      source={source}
      label={`日別の${m.name}（単位: ${m.unit}）`}
      minHeight={280}
    />
  );
}
