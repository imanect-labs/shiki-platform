"use client";

import * as React from "react";

import { cn } from "@/lib/utils";
import {
  clampWidth,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  useSidebar,
} from "./sidebar-context";

const STEP = 16; // キーボード操作の 1 ステップ（px）

/// サイドバー右端のリサイズハンドル。
/// - ドラッグ中は aside 幅を DOM に直接書き込み（再レンダしない）、離した時に確定。
/// - キーボード操作可（← → で増減、Home/End で最小/最大、Enter/ダブルクリックで既定へ）。
export function SidebarResizer({
  targetRef,
}: {
  targetRef: React.RefObject<HTMLElement | null>;
}) {
  const { width, setWidth, resetWidth } = useSidebar();
  const [dragging, setDragging] = React.useState(false);
  const dragState = React.useRef<{ startX: number; startWidth: number } | null>(null);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const el = targetRef.current;
    if (!el) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    dragState.current = { startX: e.clientX, startWidth: el.getBoundingClientRect().width };
    setDragging(true);
    // ドラッグ中は幅 transition を切って 1:1 追従にする。
    el.style.transition = "none";
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const st = dragState.current;
    const el = targetRef.current;
    if (!st || !el) return;
    const next = clampWidth(st.startWidth + (e.clientX - st.startX));
    // 再レンダせず DOM へ直書き（1:1 の追従）。
    el.style.width = `${next}px`;
  };

  const finishDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = targetRef.current;
    if (!dragState.current || !el) return;
    dragState.current = null;
    setDragging(false);
    el.style.transition = "";
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    // 確定値を state/localStorage に反映（以後は inline style と一致）。
    setWidth(el.getBoundingClientRect().width);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    switch (e.key) {
      case "ArrowLeft":
        e.preventDefault();
        setWidth(width - STEP);
        break;
      case "ArrowRight":
        e.preventDefault();
        setWidth(width + STEP);
        break;
      case "Home":
        e.preventDefault();
        setWidth(SIDEBAR_MIN_WIDTH);
        break;
      case "End":
        e.preventDefault();
        setWidth(SIDEBAR_MAX_WIDTH);
        break;
      case "Enter":
        e.preventDefault();
        resetWidth();
        break;
    }
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="サイドバーの幅"
      aria-valuemin={SIDEBAR_MIN_WIDTH}
      aria-valuemax={SIDEBAR_MAX_WIDTH}
      aria-valuenow={width}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={finishDrag}
      onPointerCancel={finishDrag}
      onKeyDown={onKeyDown}
      onDoubleClick={resetWidth}
      className={cn(
        "group absolute -right-1.5 top-0 z-20 flex h-full w-3 cursor-col-resize touch-none items-stretch justify-center outline-none",
        "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
      )}
    >
      {/* 視覚的なヒント線（ホバー/ドラッグ/フォーカスで現れる） */}
      <span
        aria-hidden
        className={cn(
          "h-full w-px bg-transparent transition-colors",
          "group-hover:bg-sidebar-ring/60 group-focus-visible:bg-sidebar-ring",
          dragging && "bg-sidebar-ring",
        )}
      />
    </div>
  );
}
