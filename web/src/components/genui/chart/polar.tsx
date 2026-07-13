"use client";

/// 極座標系レンダラ（radar / radial_bar）。

import * as React from "react";

import {
  LabelList,
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
      {/* 太い色付き輪郭＋頂点ドットで系列を区別する（淡い塗りの重なりで濁らせない）。 */}
      {series.map((name, i) => (
        <Radar
          key={name}
          dataKey={name}
          stroke={colorFor(i)}
          strokeWidth={2.25}
          fill={colorFor(i)}
          fillOpacity={0.14}
          dot={{ r: 2.5, fill: colorFor(i), strokeWidth: 0 }}
          isAnimationActive={false}
        />
      ))}
    </RadarChart>
  );
}

/// 放射状バー: x ラベルごとの合算値をリングで比較（ゲージ/進捗向け）。
/// 値域は最大値（%指標なら 100）を満端とし、各リング端に値ラベルを出す。
export function renderRadialBar(spec: ChartSpec): React.ReactElement {
  const totals = toTotals(spec).map((d, i) => ({ ...d, fill: colorFor(i) }));
  const max = Math.max(100, ...totals.map((d) => d.value));
  return (
    <RadialBarChart
      data={totals}
      innerRadius="28%"
      outerRadius="100%"
      startAngle={90}
      endAngle={-270}
      barSize={13}
    >
      {/* 角度軸を [0, max] に固定＝各リングの塗り = value/max（ゲージ表現）。 */}
      <PolarAngleAxis type="number" domain={[0, max]} tick={false} axisLine={false} />
      <Tooltip contentStyle={TOOLTIP_STYLE} wrapperStyle={{ outline: "none" }} />
      <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12, paddingTop: 6 }} />
      <RadialBar
        dataKey="value"
        background={{ fill: "var(--secondary)" }}
        cornerRadius={7}
        isAnimationActive={false}
      >
        <LabelList
          dataKey="value"
          position="insideStart"
          fill="var(--card)"
          fontSize={11}
          fontWeight={600}
          formatter={(value) => Number(value).toLocaleString("ja-JP")}
        />
      </RadialBar>
    </RadialBarChart>
  );
}
