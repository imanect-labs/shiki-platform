"use client";

/// 管理画面のグラフを、チャットの本文の図と同じ Yui（@imanect/yui）で描く（#783）。
///
/// 色・棒の太さ・角丸・目盛り線・凡例・ホバーの値の出し方まで本文の図と揃えるため、自前で描かず
/// ```ui の記法（bar・line・funnel）を組み立てて Yui に渡す。ラベルは画面側の固定文字列と日付だけを
/// 使い、利用者が入力した文字（部署名など）は記法に入れない（記法の区切りや記号を壊さないため）。
/// 描画は重いので遅延読込する（管理画面の枠と数値を先に出す・perf.md A）。

import * as React from "react";
import dynamic from "next/dynamic";

const Figure = dynamic(() => import("./yui-chart-figure"), {
  ssr: false,
  loading: () => <div className="animate-pulse rounded-md bg-muted/60" />,
});

/// 数値のセル（記法は単位を付けない素の数値）。
export function yuiNumber(n: number, digits = 0): string {
  return Number.isFinite(n) ? n.toFixed(digits).replace(/\.0+$|(\.\d*?)0+$/, "$1") : "0";
}

/// `label` は図の名前（読み上げ用）。図の見出しはパネル側にあるので、記法には見出しを書かない。
/// `minHeight` は描画後のおおよその高さ。読み込み中も同じ高さを取り、表示のずれを抑える。
export function YuiChart({
  source,
  label,
  minHeight,
  className,
}: {
  source: string;
  label: string;
  minHeight: number;
  className?: string;
}) {
  return (
    // grid: 読み込み中の枠（高さ指定なし）を minHeight いっぱいに伸ばす。
    <figure aria-label={label} className="m-0 grid" style={{ minHeight }}>
      <Figure source={source} className={className} />
    </figure>
  );
}
