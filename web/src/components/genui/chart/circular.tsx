"use client";

/// 円系レンダラ（pie / donut）。x ラベルごとに全系列を合算した構成比で描く。
/// スライスに構成比（%）を、ドーナツ中央に合計を出して「読める」円グラフにする。

import * as React from "react";

import { Cell, Legend, Pie, PieChart, Tooltip } from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { colorFor, TOOLTIP_STYLE, toTotals } from "./palette";

const RAD = Math.PI / 180;

/// スライス内の構成比ラベル（小さすぎるスライスは省く）。
function sliceLabel(props: {
  cx?: number;
  cy?: number;
  midAngle?: number;
  innerRadius?: number;
  outerRadius?: number;
  percent?: number;
}): React.ReactElement | null {
  const { cx = 0, cy = 0, midAngle = 0, innerRadius = 0, outerRadius = 0, percent = 0 } = props;
  if (percent < 0.08) return null;
  const r = innerRadius + (outerRadius - innerRadius) * (innerRadius > 0 ? 0.5 : 0.62);
  const x = cx + r * Math.cos(-midAngle * RAD);
  const y = cy + r * Math.sin(-midAngle * RAD);
  return (
    <text
      x={x}
      y={y}
      fill="var(--card)"
      fontSize={11}
      fontWeight={600}
      textAnchor="middle"
      dominantBaseline="central"
      style={{ pointerEvents: "none" }}
    >
      {Math.round(percent * 100)}%
    </text>
  );
}

export function renderPie(spec: ChartSpec, donut: boolean): React.ReactElement {
  const data = toTotals(spec).map((d, i) => ({ ...d, fill: colorFor(i) }));
  return (
    <PieChart>
      <Tooltip contentStyle={TOOLTIP_STYLE} wrapperStyle={{ outline: "none" }} />
      <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12, paddingTop: 8 }} />
      <Pie
        data={data}
        dataKey="value"
        nameKey="name"
        innerRadius={donut ? "60%" : 0}
        outerRadius="82%"
        paddingAngle={donut ? 2 : 1}
        stroke="var(--card)"
        strokeWidth={donut ? 2 : 1.5}
        labelLine={false}
        label={sliceLabel}
        isAnimationActive={false}
      >
        {data.map((d) => (
          <Cell key={d.name} fill={d.fill} />
        ))}
      </Pie>
    </PieChart>
  );
}
