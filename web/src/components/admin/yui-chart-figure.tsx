"use client";

/// Yui の図の本体。yui-chart.tsx が `next/dynamic` で遅延読込する。

import * as React from "react";
import { parseBlock } from "@imanect/yui";
import { YuiFigure } from "@imanect/yui/react";

import { cn } from "@/lib/utils";

export default function YuiChartFigure({
  source,
  className,
}: {
  source: string;
  className?: string;
}) {
  const figure = React.useMemo(() => parseBlock(source, { final: true }).figure, [source]);
  if (!figure) return null;
  // admin-yui: 図の地をパネルの面（card）に合わせる（globals.css）。
  return <YuiFigure figure={figure} className={cn("admin-yui", className)} />;
}
