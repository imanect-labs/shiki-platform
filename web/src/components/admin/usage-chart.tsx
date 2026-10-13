"use client";

import * as React from "react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

import { AXIS_TICK, TOOLTIP_STYLE } from "@/components/genui/chart/palette";
import { formatCompact, formatDay, formatInt, formatUsd } from "./format";

export type UsageMetric = "cost" | "tokens" | "calls";

type Day = { day: string; cost_usd_micros: number; tokens: number; calls: number };

const VALUE: Record<UsageMetric, (d: Day) => number> = {
  cost: (d) => d.cost_usd_micros,
  tokens: (d) => d.tokens,
  calls: (d) => d.calls,
};

const FORMAT: Record<UsageMetric, (n: number) => string> = {
  cost: formatUsd,
  tokens: (n) => `${formatCompact(n)} トークン`,
  calls: (n) => `${formatInt(n)} 回`,
};

/// コストの目盛り。最大値の桁に合わせて小数を決め、目盛りが同じ表記に潰れないようにする。
function costTick(maxMicros: number): (micros: number) => string {
  const maxUsd = maxMicros / 1_000_000;
  const digits = maxUsd < 1 ? 2 : 0;
  return (micros) => {
    const usd = micros / 1_000_000;
    if (usd === 0) return "$0";
    // 1 ドル未満は有効数字 2 桁（$0.00015 と $0.0001 が同じ表記に潰れない）。
    return digits === 0 ? `$${formatCompact(usd)}` : `$${Number(usd.toPrecision(2))}`;
  };
}

function axisFormatter(metric: UsageMetric, max: number): (n: number) => string {
  return metric === "cost" ? costTick(max) : formatCompact;
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
    <div role="img" aria-label={label} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={rows} margin={{ top: 8, right: 4, bottom: 0, left: 0 }} barCategoryGap={2}>
          <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
          <XAxis
            dataKey="day"
            tick={AXIS_TICK}
            tickLine={false}
            axisLine={{ stroke: "var(--border)" }}
            tickFormatter={(d: string) => {
              const [, m, dd] = d.split("-").map(Number);
              return `${m}/${dd}`;
            }}
            interval="preserveStartEnd"
            minTickGap={24}
          />
          <YAxis
            tick={AXIS_TICK}
            tickLine={false}
            axisLine={false}
            width={56}
            tickFormatter={axisFormatter(metric, Math.max(...rows.map((r) => r.value)))}
            allowDecimals={false}
          />
          <Tooltip
            contentStyle={TOOLTIP_STYLE}
            cursor={{ fill: "var(--secondary)", opacity: 0.5 }}
            wrapperStyle={{ outline: "none" }}
            labelFormatter={(d) => formatDay(String(d))}
            formatter={(v) => [FORMAT[metric](Number(v)), ""]}
            separator=""
          />
          <Bar dataKey="value" fill="var(--primary)" radius={[4, 4, 0, 0]} maxBarSize={28} />
        </BarChart>
      </ResponsiveContainer>
    </div>
  );
}
