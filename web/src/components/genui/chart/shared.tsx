"use client";

/// カルテシアン系チャート（bar/line/area/combo/scatter）で共有する軸・グリッド・凡例。

import * as React from "react";

import { CartesianGrid, Legend, Tooltip, XAxis, YAxis } from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { AXIS_TICK, TOOLTIP_STYLE } from "./palette";

/// 共通の軸・グリッド・ツールチップ・凡例（系列が複数のときだけ凡例を出す）。
export function CartesianAxes({
  spec,
  seriesCount,
}: {
  spec: ChartSpec;
  seriesCount: number;
}) {
  return (
    <>
      <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
      <XAxis
        dataKey="x"
        type="category"
        tick={AXIS_TICK}
        tickLine={false}
        axisLine={{ stroke: "var(--border)" }}
        label={
          spec.x_label
            ? { value: spec.x_label, position: "insideBottom", dy: 8, fontSize: 11 }
            : undefined
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
      <Tooltip
        contentStyle={TOOLTIP_STYLE}
        cursor={{ fill: "var(--secondary)", opacity: 0.45 }}
        wrapperStyle={{ outline: "none" }}
      />
      {seriesCount > 1 ? (
        <Legend
          iconType="circle"
          iconSize={8}
          wrapperStyle={{ fontSize: 12, paddingTop: 10 }}
        />
      ) : null}
    </>
  );
}

/// 面/バーのグラデーション定義（四季トークン→透明）。id は系列インデックスで一意。
/// `from`/`to` で上端/下端の不透明度を渡せる（面は淡く、バーは濃いめに使う）。
export function GradientDefs({
  ids,
  from = 0.35,
  to = 0.02,
}: {
  ids: { id: string; color: string }[];
  from?: number;
  to?: number;
}) {
  return (
    <defs>
      {ids.map(({ id, color }) => (
        <linearGradient key={id} id={id} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity={from} />
          <stop offset="100%" stopColor={color} stopOpacity={to} />
        </linearGradient>
      ))}
    </defs>
  );
}
