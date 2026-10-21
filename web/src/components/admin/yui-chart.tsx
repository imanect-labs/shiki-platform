"use client";

/// 管理画面のグラフを、チャットの本文の図と同じ Yui（@imanect/yui）で描く（#783）。
///
/// 色・棒の太さ・角丸・目盛り線・凡例・ホバーの値の出し方まで本文の図と揃えるため、自前で描かず
/// ```ui の記法（bar・line・funnel）を組み立てて Yui に渡す。Yui は文字と数値だけを描く（HTML・URL・
/// スクリプトを持たない）ので、ラベルの「|」と改行だけ潰せば安全に渡せる。
/// 描画は重いので遅延読込する（管理画面の枠と数値を先に出す・perf.md A）。

import * as React from "react";
import dynamic from "next/dynamic";

const Figure = dynamic(() => import("./yui-chart-figure"), {
  ssr: false,
  loading: () => <div className="h-[260px] animate-pulse rounded-md bg-muted/60" />,
});

/// 記法の 1 セルに入れる文字（表の区切り「|」と改行を潰す）。
export function yuiCell(text: string): string {
  return text.replace(/[|\r\n]+/g, " ").trim() || "—";
}

/// 数値のセル（記法は単位を付けない素の数値）。
export function yuiNumber(n: number, digits = 0): string {
  return Number.isFinite(n) ? n.toFixed(digits).replace(/\.0+$|(\.\d*?)0+$/, "$1") : "0";
}

export function YuiChart({ source, className }: { source: string; className?: string }) {
  return <Figure source={source} className={className} />;
}
