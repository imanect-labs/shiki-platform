"use client";

/// 極座標系レンダラ（radar / radial_bar）。

import * as React from "react";

import {
  Legend,
  PolarAngleAxis,
  PolarGrid,
  PolarRadiusAxis,
  Radar,
  RadarChart,
  RadialBar,
  RadialBarChart,
  Tooltip,
} from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { AXIS_TICK, colorFor, type Row, TOOLTIP_STYLE, toTotals } from "./palette";

/// レーダー: x を項目軸、各系列を多角形として重ねる。
export function renderRadar(spec: ChartSpec, rows: Row[], series: string[]): React.ReactElement {
  return (
    <RadarChart data={rows} outerRadius="72%">
      <PolarGrid stroke="var(--border)" />
      <PolarAngleAxis dataKey="x" tick={AXIS_TICK} />
      {/* 半径目盛りのラベルは中央で重なり煩雑なので非表示（形状比較が主目的）。 */}
      <PolarRadiusAxis tick={false} axisLine={false} tickLine={false} />
      <Tooltip contentStyle={TOOLTIP_STYLE} />
      {series.length > 1 ? (
        <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12 }} />
      ) : null}
      {series.map((name, i) => (
        <Radar
          key={name}
          dataKey={name}
          stroke={colorFor(i)}
          fill={colorFor(i)}
          fillOpacity={0.2}
          isAnimationActive={false}
        />
      ))}
    </RadarChart>
  );
}

/// 放射状バー: x ラベルごとの合算値をリングで比較（ゲージ/進捗向け）。
export function renderRadialBar(spec: ChartSpec): React.ReactElement {
  const data = toTotals(spec).map((d, i) => ({ ...d, fill: colorFor(i) }));
  return (
    <RadialBarChart data={data} innerRadius="25%" outerRadius="95%" startAngle={90} endAngle={-270}>
      <Tooltip contentStyle={TOOLTIP_STYLE} />
      <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12 }} />
      <RadialBar
        dataKey="value"
        background={{ fill: "var(--secondary)" }}
        cornerRadius={6}
        isAnimationActive={false}
      />
    </RadialBarChart>
  );
}
