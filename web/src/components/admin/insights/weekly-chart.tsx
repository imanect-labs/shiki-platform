"use client";

import * as React from "react";
import dynamic from "next/dynamic";

import type { InsightsOverview } from "@/lib/insights-api";
import { addDays } from "../format";

export type WeekRow = {
  week: string;
  heavy: number;
  regular: number;
  light: number;
  /// 期間に一部の日だけ含まれる週（人数が少なく見えるのは途中だから）。
  partial: boolean;
};

/// 使った日数の区分（順序のある 3 段階なので `--primary` の濃淡 1 色で表す）。
export const WEEKLY_SERIES = [
  { key: "heavy", label: "ヘビー（週 4 日以上）", color: "var(--primary)" },
  {
    key: "regular",
    label: "レギュラー（週 2〜3 日）",
    color: "var(--ordinal-2)",
  },
  {
    key: "light",
    label: "ライト（週 1 日）",
    color: "var(--ordinal-3)",
  },
] as const;

const Bars = dynamic(() => import("./weekly-chart-bars"), {
  ssr: false,
  loading: () => <div className="h-full animate-pulse rounded-md bg-muted/60" />,
});

/// 週ごとのアクティブ利用者（凡例つき）。
export function WeeklyChart({
  weeks,
  from,
  to,
  height = 220,
}: {
  weeks: InsightsOverview["weekly"];
  /// 期間（両端含む）。週の一部だけが入る端の週を見分ける。
  from: string;
  to: string;
  height?: number;
}) {
  const rows: WeekRow[] = weeks.map((w) => ({
    week: w.week_start,
    heavy: w.heavy,
    regular: w.regular,
    light: w.light,
    partial: w.week_start < from || addDays(w.week_start, 6) > to,
  }));
  const anyPartial = rows.some((r) => r.partial);
  const total = rows.reduce((n, r) => n + r.heavy + r.regular + r.light, 0);
  return (
    <div className="flex flex-col gap-3">
      <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        {WEEKLY_SERIES.map((s) => (
          <li key={s.key} className="inline-flex items-center gap-1.5">
            <span className="size-2.5 rounded-[3px]" style={{ backgroundColor: s.color }} aria-hidden />
            {s.label}
          </li>
        ))}
        {anyPartial ? <li className="md:ml-auto">* 期間に一部の日だけ含まれる週</li> : null}
      </ul>
      {total === 0 ? (
        <div
          className="flex items-center justify-center rounded-md border border-dashed text-xs text-muted-foreground"
          style={{ height }}
        >
          この期間に AI を使った人はいません
        </div>
      ) : (
        <div style={{ height }}>
          <Bars
            rows={rows}
            height={height}
            label={`週ごとのアクティブ利用者。${rows
              .map((r) => `${r.week} の週 ${r.heavy + r.regular + r.light} 人`)
              .join("、")}`}
          />
        </div>
      )}
    </div>
  );
}
