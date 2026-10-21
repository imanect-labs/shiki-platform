"use client";

/// ピンチで拡大・縮小する（スマホの 2 本指・トラックパッドのピンチ／Ctrl+ホイール）。
///
/// 操作中はページを描き直さず、中身に CSS の transform を掛けて見た目だけ拡大する（重い再描画を
/// 指の動きのたびに走らせない）。指を離した（ホイールが止まった）ところで倍率を確定し、
/// `onCommit` に倍率の比と焦点（スクロール枠内の座標）を渡す。焦点の位置を保つのは呼び出し側。
import * as React from "react";

/// ホイールが止まったとみなすまでの時間。
const WHEEL_IDLE_MS = 160;

export function usePinchZoom({
  rootRef,
  contentRef,
  clamp,
  onCommit,
  enabled,
}: {
  /// 枠と中身が描かれてから true にする（読み込み中は要素が無い）。
  enabled: boolean;
  rootRef: React.RefObject<HTMLElement | null>;
  contentRef: React.RefObject<HTMLElement | null>;
  /// 比を許される範囲に収める（今の倍率 × 比 が上限・下限を超えないように）。
  clamp: (ratio: number) => number;
  onCommit: (ratio: number, focal: { x: number; y: number }) => void;
}) {
  const latest = React.useRef({ clamp, onCommit });
  latest.current = { clamp, onCommit };

  React.useEffect(() => {
    const root = rootRef.current;
    if (!root || !enabled) return;
    let ratio = 1;
    let focal = { x: 0, y: 0 };
    let active = false;
    let wheelTimer: number | null = null;

    const preview = () => {
      const content = contentRef.current;
      if (!content) return;
      // 焦点（スクロール枠内の座標）を中身の座標に直して、そこを中心に拡大する。
      const ox = root.scrollLeft + focal.x - content.offsetLeft;
      const oy = root.scrollTop + focal.y - content.offsetTop;
      content.style.transformOrigin = `${ox}px ${oy}px`;
      content.style.transform = `scale(${ratio})`;
    };
    const begin = (x: number, y: number) => {
      const r = root.getBoundingClientRect();
      focal = { x: x - r.left, y: y - r.top };
      ratio = 1;
      active = true;
    };
    const end = () => {
      if (!active) return;
      active = false;
      const content = contentRef.current;
      if (content) {
        content.style.transform = "";
        content.style.transformOrigin = "";
      }
      if (Math.abs(ratio - 1) > 0.01) latest.current.onCommit(ratio, focal);
      ratio = 1;
    };

    // トラックパッドのピンチ（Chrome/Safari は ctrlKey 付きの wheel になる）と Ctrl+ホイール。
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      if (!active) begin(e.clientX, e.clientY);
      // トラックパッドは小さな delta が続き、マウスのホイールは 1 刻みで大きく飛ぶ。1 回ぶんを抑えて揃える。
      const d = Math.max(-25, Math.min(25, e.deltaY));
      ratio = latest.current.clamp(ratio * Math.exp(-d * 0.01));
      preview();
      if (wheelTimer != null) window.clearTimeout(wheelTimer);
      wheelTimer = window.setTimeout(end, WHEEL_IDLE_MS);
    };

    let startDist = 0;
    const dist = (t: TouchList) => Math.hypot(t[0].clientX - t[1].clientX, t[0].clientY - t[1].clientY);
    const onTouchStart = (e: TouchEvent) => {
      if (e.touches.length !== 2) return;
      startDist = dist(e.touches);
      begin((e.touches[0].clientX + e.touches[1].clientX) / 2, (e.touches[0].clientY + e.touches[1].clientY) / 2);
    };
    const onTouchMove = (e: TouchEvent) => {
      if (!active || e.touches.length !== 2 || startDist === 0) return;
      e.preventDefault();
      ratio = latest.current.clamp(dist(e.touches) / startDist);
      preview();
    };
    const onTouchEnd = (e: TouchEvent) => {
      if (e.touches.length < 2) end();
    };

    root.addEventListener("wheel", onWheel, { passive: false });
    root.addEventListener("touchstart", onTouchStart, { passive: true });
    root.addEventListener("touchmove", onTouchMove, { passive: false });
    root.addEventListener("touchend", onTouchEnd);
    root.addEventListener("touchcancel", onTouchEnd);
    return () => {
      if (wheelTimer != null) window.clearTimeout(wheelTimer);
      root.removeEventListener("wheel", onWheel);
      root.removeEventListener("touchstart", onTouchStart);
      root.removeEventListener("touchmove", onTouchMove);
      root.removeEventListener("touchend", onTouchEnd);
      root.removeEventListener("touchcancel", onTouchEnd);
    };
  }, [rootRef, contentRef, enabled]);
}
