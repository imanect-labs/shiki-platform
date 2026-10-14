"use client";

/// 日別の棒グラフ本体（recharts）。重いので usage-chart.tsx が `next/dynamic` で遅延読込する
/// （管理画面の枠・数値タイル・管理者チェックを先に描く・perf.md A）。

import * as React from "react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

import { AXIS_TICK, TOOLTIP_STYLE } from "@/components/genui/chart/palette";
import { formatCompact, formatDay, formatInt, formatUsd } from "./format";
import type { UsageMetric } from "./usage-chart";

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

export default function UsageChartBars({
  rows,
  metric,
  height,
  label,
}: {
  rows: { day: string; value: number }[];
  metric: UsageMetric;
  height: number;
  label: string;
}) {
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
