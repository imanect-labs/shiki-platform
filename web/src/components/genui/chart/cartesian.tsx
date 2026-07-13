"use client";

/// カルテシアン系レンダラ（bar / line / area / combo・stacked 対応）。

import * as React from "react";

import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  ComposedChart,
  Line,
  LineChart,
} from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { colorFor, type Row } from "./palette";
import { CartesianAxes, GradientDefs } from "./shared";

const MARGIN = { top: 8, right: 12, bottom: 4, left: 0 } as const;

export function renderBar(spec: ChartSpec, rows: Row[], series: string[]): React.ReactElement {
  const stackId = spec.stacked ? "stack" : undefined;
  return (
    <BarChart data={rows} margin={MARGIN}>
      <CartesianAxes spec={spec} seriesCount={series.length} />
      {series.map((name, i) => (
        <Bar
          key={name}
          dataKey={name}
          stackId={stackId}
          fill={colorFor(i)}
          radius={spec.stacked ? [0, 0, 0, 0] : [4, 4, 0, 0]}
          maxBarSize={48}
          isAnimationActive={false}
        />
      ))}
    </BarChart>
  );
}

export function renderLine(spec: ChartSpec, rows: Row[], series: string[]): React.ReactElement {
  return (
    <LineChart data={rows} margin={MARGIN}>
      <CartesianAxes spec={spec} seriesCount={series.length} />
      {series.map((name, i) => (
        <Line
          key={name}
          type="monotone"
          dataKey={name}
          stroke={colorFor(i)}
          strokeWidth={2}
          dot={{ r: 2.5 }}
          activeDot={{ r: 4 }}
          isAnimationActive={false}
        />
      ))}
    </LineChart>
  );
}

export function renderArea(spec: ChartSpec, rows: Row[], series: string[]): React.ReactElement {
  const stackId = spec.stacked ? "stack" : undefined;
  const grads = series.map((name, i) => ({ id: `genui-area-${i}`, color: colorFor(i) }));
  return (
    <AreaChart data={rows} margin={MARGIN}>
      <GradientDefs ids={grads} />
      <CartesianAxes spec={spec} seriesCount={series.length} />
      {series.map((name, i) => (
        <Area
          key={name}
          type="monotone"
          dataKey={name}
          stackId={stackId}
          stroke={colorFor(i)}
          fill={`url(#genui-area-${i})`}
          strokeWidth={2}
          isAnimationActive={false}
        />
      ))}
    </AreaChart>
  );
}

/// 複合: line_series に列挙された系列を line、それ以外を bar で描く。
export function renderCombo(spec: ChartSpec, rows: Row[], series: string[]): React.ReactElement {
  const asLine = new Set(spec.line_series ?? []);
  return (
    <ComposedChart data={rows} margin={MARGIN}>
      <CartesianAxes spec={spec} seriesCount={series.length} />
      {series.map((name, i) =>
        asLine.has(name) ? (
          <Line
            key={name}
            type="monotone"
            dataKey={name}
            stroke={colorFor(i)}
            strokeWidth={2}
            dot={{ r: 2.5 }}
            isAnimationActive={false}
          />
        ) : (
          <Bar
            key={name}
            dataKey={name}
            fill={colorFor(i)}
            radius={[4, 4, 0, 0]}
            maxBarSize={40}
            isAnimationActive={false}
          />
        ),
      )}
    </ComposedChart>
  );
}
