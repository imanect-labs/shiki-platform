"use client";

/// 円系レンダラ（pie / donut）。x ラベルごとに全系列を合算した構成比で描く。

import * as React from "react";

import { Cell, Legend, Pie, PieChart, Tooltip } from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { colorFor, TOOLTIP_STYLE, toTotals } from "./palette";

export function renderPie(spec: ChartSpec, donut: boolean): React.ReactElement {
  const data = toTotals(spec).map((d, i) => ({ ...d, fill: colorFor(i) }));
  return (
    <PieChart>
      <Tooltip contentStyle={TOOLTIP_STYLE} />
      <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12 }} />
      <Pie
        data={data}
        dataKey="value"
        nameKey="name"
        innerRadius={donut ? "58%" : 0}
        outerRadius="80%"
        paddingAngle={donut ? 2 : 0}
        stroke="var(--card)"
        strokeWidth={donut ? 2 : 1}
        isAnimationActive={false}
      >
        {data.map((d) => (
          <Cell key={d.name} fill={d.fill} />
        ))}
      </Pie>
    </PieChart>
  );
}
