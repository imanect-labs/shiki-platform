"use client";

import * as React from "react";

/// 自分の発話の吹き出しに付ける印（`[data-user-turn]`）。
export const USER_TURN_ATTR = "data-user-turn";

/// 読んでいる位置を「越えた」とみなす遊び（px）。ちょうど先頭に合わせた発話を前後と取り違えない。
const SLOP_PX = 4;

/// 会話の「一つ前／一つ後の自分の発話へ」移動（チャット専用）。
///
/// 基準線は本文の上端の余白（浮いたクロームの下・最初の発話が来る位置）。移動先の発話をそこへ
/// 合わせるので、どの発話へ飛んでも最初の発話と同じ位置に見える。移動は `scrollTop` だけを動かす
/// （`scrollIntoView` は祖先のスクロール領域まで動かすため使わない・use-stick-to-bottom と同じ）。
export function useTurnJump(scroller: HTMLElement | null, content: HTMLElement | null) {
  const [can, setCan] = React.useState({ prev: false, next: false });

  // 各発話の、スクロール領域の先頭からの位置と基準線。
  const measure = React.useCallback(() => {
    if (!scroller || !content) return null;
    const box = scroller.getBoundingClientRect();
    const tops = Array.from(content.querySelectorAll<HTMLElement>(`[${USER_TURN_ATTR}]`)).map(
      (el) => el.getBoundingClientRect().top - box.top + scroller.scrollTop,
    );
    const inset = parseFloat(getComputedStyle(content).paddingTop) || 0;
    return { tops, line: scroller.scrollTop + inset, inset };
  }, [scroller, content]);

  const refresh = React.useCallback(() => {
    const m = measure();
    if (!m) return;
    const maxTop = scroller!.scrollHeight - scroller!.clientHeight;
    setCan({
      prev: m.tops.some((t) => t < m.line - SLOP_PX),
      // 下端まで来ていれば、それより下の発話へはもう動かせない。
      next: scroller!.scrollTop < maxTop - SLOP_PX && m.tops.some((t) => t > m.line + SLOP_PX),
    });
  }, [measure, scroller]);

  React.useEffect(() => {
    if (!scroller) return;
    refresh();
    scroller.addEventListener("scroll", refresh, { passive: true });
    // 発話が増えた・本文が伸びた（スクロールせずに前後が変わる）ときも測り直す。
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(refresh);
    if (content) ro?.observe(content);
    return () => {
      scroller.removeEventListener("scroll", refresh);
      ro?.disconnect();
    };
  }, [scroller, content, refresh]);

  const jump = React.useCallback(
    (dir: "prev" | "next") => {
      const m = measure();
      if (!m || !scroller) return;
      const target =
        dir === "prev"
          ? m.tops.filter((t) => t < m.line - SLOP_PX).at(-1)
          : m.tops.find((t) => t > m.line + SLOP_PX);
      if (target === undefined) return;
      scroller.scrollTo({ top: Math.max(0, target - m.inset), behavior: "smooth" });
    },
    [measure, scroller],
  );

  return {
    canPrev: can.prev,
    canNext: can.next,
    jumpPrev: React.useCallback(() => jump("prev"), [jump]),
    jumpNext: React.useCallback(() => jump("next"), [jump]),
  };
}
