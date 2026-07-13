"use client";

/// generative UI の KPI スタットタイル（数値＋前期比デルタ＋インライン sparkline）。
/// 配色は四季トークン。delta 正=改善（↑・緑）／負=悪化（↓・赤）。

import * as React from "react";

import { TrendingDown, TrendingUp } from "lucide-react";

import type { StatProps } from "@/generated/gui-spec";
import { cn } from "@/lib/utils";

/// number[] → 高さ h・幅 w に正規化した polyline points 文字列。
function sparklinePoints(values: number[], w: number, h: number): string {
  const finite = values.filter((v) => Number.isFinite(v));
  if (finite.length < 2) return "";
  const min = Math.min(...finite);
  const max = Math.max(...finite);
  const span = max - min || 1;
  const step = w / (finite.length - 1);
  return finite
    .map((v, i) => `${(i * step).toFixed(1)},${(h - ((v - min) / span) * h).toFixed(1)}`)
    .join(" ");
}

export function GenUiStat({ stat }: { stat: StatProps }) {
  const { label, value, unit, delta, delta_label, trend, caption } = stat;
  const hasDelta = typeof delta === "number" && Number.isFinite(delta);
  const up = hasDelta && (delta as number) >= 0;
  const points = React.useMemo(() => sparklinePoints(trend ?? [], 96, 28), [trend]);
  const strokeColor = up ? "var(--season-summer)" : hasDelta ? "var(--destructive)" : "var(--primary)";

  return (
    <figure
      className="min-w-0 rounded-xl border border-border bg-card/60 p-3.5"
      data-testid="genui-stat"
    >
      <figcaption className="truncate text-[12px] font-medium text-muted-foreground">{label}</figcaption>
      <div className="mt-1 flex items-end justify-between gap-3">
        <div className="flex items-baseline gap-1 min-w-0">
          <span className="text-2xl font-semibold tabular-nums tracking-tight text-foreground">
            {value}
          </span>
          {unit ? <span className="text-[13px] text-muted-foreground">{unit}</span> : null}
        </div>
        {points ? (
          <svg
            width={96}
            height={28}
            viewBox="0 0 96 28"
            className="shrink-0 overflow-visible"
            aria-hidden
          >
            <polyline
              points={points}
              fill="none"
              stroke={strokeColor}
              strokeWidth={1.75}
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        ) : null}
      </div>
      {(hasDelta || caption) && (
        <div className="mt-1.5 flex items-center gap-2">
          {hasDelta ? (
            <span
              className={cn(
                "inline-flex items-center gap-0.5 text-[12px] font-medium tabular-nums",
                up ? "text-[var(--season-summer)]" : "text-destructive",
              )}
            >
              {up ? <TrendingUp className="size-3.5" /> : <TrendingDown className="size-3.5" />}
              {up ? "+" : ""}
              {(delta as number).toLocaleString(undefined, { maximumFractionDigits: 2 })}%
            </span>
          ) : null}
          {delta_label ? (
            <span className="text-[11px] text-muted-foreground">{delta_label}</span>
          ) : null}
          {caption ? (
            <span className="truncate text-[11px] text-muted-foreground">{caption}</span>
          ) : null}
        </div>
      )}
    </figure>
  );
}
