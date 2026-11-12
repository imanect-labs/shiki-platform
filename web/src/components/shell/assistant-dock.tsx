"use client";

/// 文書のアシスタントパネル（`AssistantLayout`）とシェルの上部バーをつなぐ。
///
/// PC（lg 以上）のパネルは上部バーの高さまで貫通して画面の右端に縦いっぱいに浮かぶ。
/// 上部バー（とページのツールバー）はパネルの幅ぶん右を空け、⋯ などの操作がパネルの左隣に
/// 来るようにする。パネルの開閉と幅をここでシェルの列へ渡す:
///
/// - 列に `data-assistant-open` と `--assistant-w` を載せる（子孫は CSS で避ける）。
/// - 幅つまみのドラッグ中は再描画せず、列の `--assistant-w` を直接書き換える。

import * as React from "react";

import { cn } from "@/lib/utils";

type DockState = { open: boolean; width: number };

type Dock = {
  /// `--assistant-w`・`data-resizing` を書く要素（シェルの列）。
  el: React.RefObject<HTMLDivElement | null>;
  publish: (state: DockState | null) => void;
};

const DockContext = React.createContext<Dock | null>(null);

/// シェルの本文列（上部バー＋本文）。アシスタントの開閉・幅を CSS 変数として子孫へ配る。
export function AssistantDockColumn({
  className,
  children,
}: {
  className?: string;
  children: React.ReactNode;
}) {
  const el = React.useRef<HTMLDivElement>(null);
  const [state, setState] = React.useState<DockState | null>(null);
  const dock = React.useMemo<Dock>(() => ({ el, publish: setState }), []);
  return (
    <DockContext.Provider value={dock}>
      <div
        ref={el}
        data-assistant-open={state?.open ? "" : undefined}
        style={state ? ({ "--assistant-w": `${state.width}px` } as React.CSSProperties) : undefined}
        className={cn("group/assistant", className)}
      >
        {children}
      </div>
    </DockContext.Provider>
  );
}

/// `AssistantLayout` 側: シェルの列（無ければ null）。
export function useAssistantDock(): Dock | null {
  return React.useContext(DockContext);
}

/// PC でアシスタントが開いているとき、パネルの幅ぶん右を空ける（上部バー・ページのツールバー）。
/// パネルは右端から 12px、そこからさらに 12px 空けて操作をパネルの左隣に置く。
export const AVOID_ASSISTANT = cn(
  "transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
  "group-data-[resizing]/assistant:transition-none",
  "lg:group-data-[assistant-open]/assistant:pr-[calc(var(--assistant-w)+1.5rem)]",
);
