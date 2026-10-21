"use client";

import * as React from "react";

import type { InsightsOverview } from "@/lib/insights-api";
import { addDays } from "../format";
import { YuiChart, yuiNumber } from "../yui-chart";

/// 使った日数の区分。色は Yui の系列の順（冬・秋・夏）で、利用区分の表と揃える（#783）。
export const WEEKLY_SERIES = [
  { key: "heavy", label: "ヘビー（週 4 日以上）", color: "var(--chart-1)" },
  { key: "regular", label: "レギュラー（週 2〜3 日）", color: "var(--chart-2)" },
  { key: "light", label: "ライト（週 1 日）", color: "var(--chart-3)" },
] as const;

const md = (iso: string) => {
  const [, m, d] = iso.split("-").map(Number);
  return `${m}/${d}`;
};

/// 週ごとのアクティブ利用者（使った日数で 3 区分の積み上げ）。チャットの本文の図と同じ Yui で描く。
export function WeeklyChart({
  weeks,
  from,
  to,
}: {
  weeks: InsightsOverview["weekly"];
  /// 期間（両端含む）。週の一部だけが入る端の週を見分ける。
  from: string;
  to: string;
}) {
  const total = weeks.reduce((n, w) => n + w.heavy + w.regular + w.light, 0);
  const source = React.useMemo(() => {
    const partial = (w: string) => w < from || addDays(w, 6) > to;
    const lines = [
      "bar",
      "stacked: true",
      "unit: 人",
      ...(weeks.some((w) => partial(w.week_start))
        ? ["note: * は期間に一部の日だけ含まれる週"]
        : []),
      `週 | ${WEEKLY_SERIES.map((s) => s.label).join(" | ")}`,
      ...weeks.map(
        (w) =>
          `${md(w.week_start)}${partial(w.week_start) ? "*" : ""} | ${yuiNumber(w.heavy)} | ${yuiNumber(w.regular)} | ${yuiNumber(w.light)}`,
      ),
    ];
    return lines.join("\n");
  }, [weeks, from, to]);

  if (total === 0) {
    return (
      <div className="flex h-[220px] items-center justify-center rounded-md border border-dashed text-xs text-muted-foreground">
        この期間に AI を使った人はいません
      </div>
    );
  }
  return <YuiChart source={source} />;
}
