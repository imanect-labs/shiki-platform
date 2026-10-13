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

const AXIS: Record<UsageMetric, (n: number) => string> = {
  cost: (n) => `$${formatCompact(n / 1_000_000)}`,
  tokens: formatCompact,
  calls: formatCompact,
};

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
            width={48}
            tickFormatter={AXIS[metric]}
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
