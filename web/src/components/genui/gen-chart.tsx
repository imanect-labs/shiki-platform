"use client";

/// generative UI のチャート（Task 6.6・recharts への静的マッピング）。
///
/// 検証済み ChartSpec のみを描画する。種別ごとのレンダラは `chart/` に分割し、
/// ここは frame（figure/タイトル/高さ）＋種別 dispatch に徹する。
/// 配色はテーマトークン（--season-*/--primary・ライト/ダーク両対応）から取る。

import * as React from "react";

import { ResponsiveContainer } from "recharts";

import type { ChartSpec } from "@/generated/gui-spec";
import { toRows } from "./chart/palette";
import { renderArea, renderBar, renderCombo, renderLine } from "./chart/cartesian";
import { renderPie } from "./chart/circular";
import { renderRadar, renderRadialBar } from "./chart/polar";
import { renderFunnel, renderScatter, renderTreemap } from "./chart/misc";

export function GenUiChart({ spec }: { spec: ChartSpec }) {
  const { rows, series } = React.useMemo(() => toRows(spec), [spec]);
  if ((spec.data?.length ?? 0) === 0) {
    return (
      <figure className="min-w-0" data-testid="genui-chart">
        <div className="flex h-64 items-center justify-center rounded-lg border border-dashed border-border bg-secondary/30 text-xs text-muted-foreground">
          データがありません
        </div>
      </figure>
    );
  }
  return (
    <figure className="min-w-0" data-testid="genui-chart" data-chart-kind={spec.kind}>
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
  rows: ReturnType<typeof toRows>["rows"],
  series: string[],
): React.ReactElement {
  switch (spec.kind) {
    case "line":
      return renderLine(spec, rows, series);
    case "area":
      return renderArea(spec, rows, series);
    case "combo":
      return renderCombo(spec, rows, series);
    case "pie":
      return renderPie(spec, false);
    case "donut":
      return renderPie(spec, true);
    case "scatter":
      return renderScatter(spec);
    case "radar":
      return renderRadar(spec, rows, series);
    case "radial_bar":
      return renderRadialBar(spec);
    case "funnel":
      return renderFunnel(spec);
    case "treemap":
      return renderTreemap(spec);
    default:
      return renderBar(spec, rows, series);
  }
}
