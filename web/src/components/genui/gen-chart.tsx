"use client";

/// generative UI のチャート（Task 6.6・recharts への静的マッピング）。
///
/// 検証済み ChartSpec（bar/line/area/pie・データ点は props 内）だけを描画する。
/// 配色はテーマトークン（--season-*/--primary・ライト/ダーク両対応）から取り、
/// 系列ごとに一貫した色を割り当てる。

import * as React from "react";

import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Legend,
  Line,
  LineChart,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";

/// カテゴリカルパレット（テーマ CSS 変数・ライト/ダークで自動追従）。
const PALETTE = [
  "var(--season-winter)",
  "var(--season-autumn)",
  "var(--season-summer)",
  "var(--season-spring)",
  "var(--primary)",
] as const;

const AXIS_TICK = { fontSize: 11, fill: "var(--muted-foreground)" } as const;
const TOOLTIP_STYLE = {
  backgroundColor: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 8,
  fontSize: 12,
  color: "var(--foreground)",
} as const;

/// 単一系列名（series 省略時の凡例ラベル）。
const DEFAULT_SERIES = "値";

/// points（x/y/series）→ recharts の行形式（x をキーに系列を列へ）。
function toRows(spec: ChartSpec): { rows: Record<string, string | number>[]; series: string[] } {
  const seriesNames: string[] = [];
  const byX = new Map<string, Record<string, string | number>>();
  for (const p of spec.data ?? []) {
    const series = p.series ?? DEFAULT_SERIES;
    if (!seriesNames.includes(series)) seriesNames.push(series);
    const row = byX.get(p.x) ?? { x: p.x };
    row[series] = p.y;
    byX.set(p.x, row);
  }
  return { rows: [...byX.values()], series: seriesNames };
}

export function GenUiChart({ spec }: { spec: ChartSpec }) {
  const { rows, series } = React.useMemo(() => toRows(spec), [spec]);
  if (rows.length === 0) {
    return <p className="text-xs text-muted-foreground">データがありません</p>;
  }
  return (
    <figure className="min-w-0" data-testid="genui-chart">
      {spec.title ? (
        <figcaption className="mb-2 text-[13px] font-semibold tracking-wide text-foreground/80">
          {spec.title}
        </figcaption>
      ) : null}
      <div className="h-64 w-full min-w-0">
        <ResponsiveContainer width="100%" height="100%">
          {renderChart(spec, rows, series)}
        </ResponsiveContainer>
      </div>
    </figure>
  );
}

function renderChart(
  spec: ChartSpec,
  rows: Record<string, string | number>[],
  series: string[],
): React.ReactElement {
  const common = {
    data: rows,
    margin: { top: 8, right: 12, bottom: 4, left: 0 },
  };
  const axes = (
    <>
      <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
      <XAxis
        dataKey="x"
        tick={AXIS_TICK}
        tickLine={false}
        axisLine={{ stroke: "var(--border)" }}
        label={
          spec.x_label ? { value: spec.x_label, position: "insideBottom", dy: 8, fontSize: 11 } : undefined
        }
      />
      <YAxis
        tick={AXIS_TICK}
        tickLine={false}
        axisLine={false}
        width={44}
        label={
          spec.y_label
            ? { value: spec.y_label, angle: -90, position: "insideLeft", fontSize: 11 }
            : undefined
        }
      />
      <Tooltip contentStyle={TOOLTIP_STYLE} cursor={{ fill: "var(--secondary)", opacity: 0.5 }} />
      {series.length > 1 ? <Legend wrapperStyle={{ fontSize: 12 }} /> : null}
    </>
  );

  switch (spec.kind) {
    case "line":
      return (
        <LineChart {...common}>
          {axes}
          {series.map((name, i) => (
            <Line
              key={name}
              type="monotone"
              dataKey={name}
              stroke={PALETTE[i % PALETTE.length]}
              strokeWidth={2}
              dot={{ r: 2.5 }}
              isAnimationActive={false}
            />
          ))}
        </LineChart>
      );
    case "area":
      return (
        <AreaChart {...common}>
          {axes}
          {series.map((name, i) => (
            <Area
              key={name}
              type="monotone"
              dataKey={name}
              stroke={PALETTE[i % PALETTE.length]}
              fill={PALETTE[i % PALETTE.length]}
              fillOpacity={0.18}
              strokeWidth={2}
              isAnimationActive={false}
            />
          ))}
        </AreaChart>
      );
    case "pie": {
      // pie は x=ラベルごとに全系列を合算した値で描く（一部の点だけ series 付きでも欠落させない）。
      const data = (rows as { x: string }[]).map((row, i) => ({
        name: row.x,
        value: series.reduce(
          (sum, name) => sum + Number((row as Record<string, unknown>)[name] ?? 0),
          0,
        ),
        fill: PALETTE[i % PALETTE.length],
      }));
      return (
        <PieChart>
          <Tooltip contentStyle={TOOLTIP_STYLE} />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          <Pie data={data} dataKey="value" nameKey="name" innerRadius="45%" isAnimationActive={false}>
            {data.map((d) => (
              <Cell key={d.name} fill={d.fill} />
            ))}
          </Pie>
        </PieChart>
      );
    }
    default:
      return (
        <BarChart {...common}>
          {axes}
          {series.map((name, i) => (
            <Bar
              key={name}
              dataKey={name}
              fill={PALETTE[i % PALETTE.length]}
              radius={[4, 4, 0, 0]}
              maxBarSize={48}
              isAnimationActive={false}
            />
          ))}
        </BarChart>
      );
  }
}
