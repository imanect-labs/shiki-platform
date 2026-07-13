"use client";

/// その他レンダラ（scatter / funnel / treemap）。

import * as React from "react";

import {
  CartesianGrid,
  Cell,
  Funnel,
  FunnelChart,
  LabelList,
  Legend,
  Scatter,
  ScatterChart,
  Tooltip,
  Treemap,
  XAxis,
  YAxis,
} from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { AXIS_TICK, colorFor, DEFAULT_SERIES, TOOLTIP_STYLE, toTotals } from "./palette";

/// 散布図: 系列ごとに点をプロット。数値 x（`xv`）があれば数値軸、無ければ
/// カテゴリ軸（`x` ラベル）で同一カテゴリを揃える（連番だと系列間で位置がずれるため）。
/// ScatterChart は XAxis/YAxis の dataKey で座標系を決めるため専用軸を持つ（共有軸は使わない）。
export function renderScatter(spec: ChartSpec): React.ReactElement {
  const numericX = (spec.data ?? []).some((p) => p.xv != null);
  const bySeries = new Map<string, { x: number; label: string; y: number }[]>();
  const order: string[] = [];
  for (const p of spec.data ?? []) {
    const name = p.series ?? DEFAULT_SERIES;
    if (!bySeries.has(name)) {
      bySeries.set(name, []);
      order.push(name);
    }
    bySeries.get(name)!.push({ x: p.xv ?? 0, label: p.x, y: p.y });
  }
  // カテゴリ軸では dataKey="label"（同一カテゴリを揃える）、数値軸では dataKey="x"（xv）。
  const xAxis = numericX ? (
    <XAxis
      type="number"
      dataKey="x"
      name={spec.x_label ?? "x"}
      tick={AXIS_TICK}
      tickLine={false}
      axisLine={{ stroke: "var(--border)" }}
    />
  ) : (
    <XAxis
      type="category"
      dataKey="label"
      allowDuplicatedCategory={false}
      name={spec.x_label ?? "x"}
      tick={AXIS_TICK}
      tickLine={false}
      axisLine={{ stroke: "var(--border)" }}
    />
  );
  return (
    <ScatterChart margin={{ top: 8, right: 16, bottom: 4, left: 0 }}>
      <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" />
      {xAxis}
      <YAxis
        type="number"
        dataKey="y"
        name={spec.y_label ?? "y"}
        width={44}
        tick={AXIS_TICK}
        tickLine={false}
        axisLine={false}
      />
      <Tooltip contentStyle={TOOLTIP_STYLE} cursor={{ strokeDasharray: "3 3", stroke: "var(--border)" }} />
      {order.length > 1 ? (
        <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12 }} />
      ) : null}
      {order.map((name, i) => (
        <Scatter key={name} name={name} data={bySeries.get(name)} fill={colorFor(i)} isAnimationActive={false} />
      ))}
    </ScatterChart>
  );
}

/// ファネル: x ラベルごとの合算値を段階として上から積む。
export function renderFunnel(spec: ChartSpec): React.ReactElement {
  const data = toTotals(spec).map((d, i) => ({ ...d, fill: colorFor(i) }));
  return (
    <FunnelChart>
      <Tooltip contentStyle={TOOLTIP_STYLE} />
      <Funnel dataKey="value" data={data} isAnimationActive={false} stroke="var(--card)">
        <LabelList position="right" fill="var(--foreground)" stroke="none" dataKey="name" fontSize={12} />
        {data.map((d) => (
          <Cell key={d.name} fill={d.fill} />
        ))}
      </Funnel>
    </FunnelChart>
  );
}

/// ツリーマップ: x ラベルごとの合算値を面積で表現（季節トークンで色分け）。
export function renderTreemap(spec: ChartSpec): React.ReactElement {
  const data = toTotals(spec).map((d) => ({ name: d.name, size: Math.max(0, d.value) }));
  return (
    <Treemap
      data={data}
      dataKey="size"
      stroke="var(--card)"
      isAnimationActive={false}
      content={<TreemapCell />}
    />
  );
}

/// ツリーマップの 1 セル（季節トークンで塗り分け、収まる場合のみラベル表示）。
function TreemapCell(props: {
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  index?: number;
  name?: string;
}) {
  const { x = 0, y = 0, width = 0, height = 0, index = 0, name = "" } = props;
  const showLabel = width > 44 && height > 20;
  return (
    <g>
      <rect x={x} y={y} width={width} height={height} fill={colorFor(index)} stroke="var(--card)" strokeWidth={2} />
      {showLabel ? (
        <text x={x + 6} y={y + 16} fontSize={11} fill="var(--card)" style={{ pointerEvents: "none" }}>
          {name}
        </text>
      ) : null}
    </g>
  );
}
