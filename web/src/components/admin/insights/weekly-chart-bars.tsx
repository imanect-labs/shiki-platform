"use client";

/// 週ごとのアクティブ利用者（使った日数で 3 区分の積み上げ）。recharts は重いので
/// weekly-chart.tsx が `next/dynamic` で遅延読込する。

import * as React from "react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";

import { AXIS_TICK, TOOLTIP_STYLE } from "@/components/genui/chart/palette";
import { formatInt } from "../format";
import { WEEKLY_SERIES, type WeekRow } from "./weekly-chart";

export default function WeeklyChartBars({
  rows,
  height,
  label,
}: {
  rows: WeekRow[];
  height: number;
  label: string;
}) {
  return (
    <div role="img" aria-label={label} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={rows} margin={{ top: 8, right: 4, bottom: 0, left: 0 }} barCategoryGap="28%">
          <CartesianGrid stroke="var(--border)" vertical={false} />
          <XAxis
            dataKey="week"
            tick={AXIS_TICK}
            tickLine={false}
            axisLine={{ stroke: "var(--border)" }}
            tickFormatter={(d: string) => {
              const [, m, dd] = d.split("-").map(Number);
              return `${m}/${dd}${rows.find((r) => r.week === d)?.partial ? "*" : ""}`;
            }}
            interval="preserveStartEnd"
            minTickGap={16}
          />
          <YAxis
            tick={AXIS_TICK}
            tickLine={false}
            axisLine={false}
            width={40}
            allowDecimals={false}
          />
          <Tooltip
            contentStyle={TOOLTIP_STYLE}
            cursor={{ fill: "var(--secondary)", opacity: 0.5 }}
            wrapperStyle={{ outline: "none" }}
            labelFormatter={(d) => {
              const [, m, dd] = String(d).split("-").map(Number);
              const partial = rows.find((r) => r.week === String(d))?.partial;
              return `${m}/${dd} からの週${partial ? "（期間に一部だけ含まれる）" : ""}`;
            }}
            formatter={(v, name) => [
              `${formatInt(Number(v))} 人`,
              WEEKLY_SERIES.find((s) => s.key === name)?.label ?? String(name),
            ]}
            itemSorter={(item) => WEEKLY_SERIES.findIndex((s) => s.key === item.dataKey)}
          />
          {/* 下から ライト → レギュラー → ヘビー。段の間は面の色で 2px 空ける。 */}
          {[...WEEKLY_SERIES].reverse().map((s, i, all) => (
            <Bar
              key={s.key}
              dataKey={s.key}
              stackId="w"
              fill={s.color}
              stroke="var(--card)"
              strokeWidth={2}
              maxBarSize={24}
              radius={i === all.length - 1 ? [4, 4, 0, 0] : 0}
              isAnimationActive={false}
            />
          ))}
        </BarChart>
      </ResponsiveContainer>
    </div>
  );
}
