"use client";

import * as React from "react";

/// 下からのシートを、指で下へスワイプして閉じられるようにする（iOS の標準シートと同じ操作感）。
///
/// - つかめる場所: 上端（取っ手の帯 `HANDLE_ZONE` px）か、シート内のスクロール領域が先頭に
///   いるときの本文。本文が途中までスクロールしている間は通常のスクロールを優先する。
/// - 入力欄・スライダー・`data-no-swipe` の中から始めた操作は奪わない（文字選択・値の調整のため）。
/// - 離した位置が高さの 25%（最大 `CLOSE_DISTANCE`）を越えるか、速く払えば閉じ、足りなければ戻す。
/// - 閉じるのは `close()`（呼び出し側が Radix の Close を押す）。`translateY` は付けたままにし、
///   退場アニメーションはその位置から下へ続く。
/// - タッチだけを対象にする（マウス・ペンは閉じるボタンや Esc で足りる）。
///
/// 戻り値の ref をシート本体（スクロール領域を含む要素）に付ける。
const HANDLE_ZONE = 44;
const START_SLOP = 8;
const CLOSE_DISTANCE = 140;
const CLOSE_VELOCITY = 0.55; // px/ms
/// 最後の動きからこれ以上止まってから離したら、払った勢いは無かったものとする。
const FLICK_STALE_MS = 100;
const NO_SWIPE = 'input, textarea, select, [contenteditable="true"], [role="slider"], [data-no-swipe]';

export function useSwipeToDismiss<T extends HTMLElement>({
  enabled,
  close,
}: {
  /// false の間は何もしない（PC 幅・横からのシートなど）。
  enabled: boolean | (() => boolean);
  close: () => void;
}): React.RefCallback<T> {
  const closeRef = React.useRef(close);
  const enabledRef = React.useRef(enabled);
  // タッチはコミット後にしか来ないので、最新の値はコミット後に写せば足りる。
  React.useLayoutEffect(() => {
    closeRef.current = close;
    enabledRef.current = enabled;
  });
  const cleanupRef = React.useRef<(() => void) | null>(null);

  return React.useCallback((node: T | null) => {
    cleanupRef.current?.();
    cleanupRef.current = null;
    if (!node) return;

    let startX = 0;
    let startY = 0;
    let lastY = 0;
    let lastT = 0;
    let velocity = 0;
    let tracking = false;
    let dragging = false;
    let fromHandle = false;
    let scrollers: HTMLElement[] = [];

    const reduceMotion = () =>
      typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    // 指の下からシート本体までにある、縦にスクロールできる要素すべて（入れ子の外側も含む）。
    const findScrollers = (from: Element | null): HTMLElement[] => {
      const out: HTMLElement[] = [];
      for (let el = from; el; el = el.parentElement) {
        if (el instanceof HTMLElement) {
          const oy = getComputedStyle(el).overflowY;
          if ((oy === "auto" || oy === "scroll") && el.scrollHeight > el.clientHeight + 1) out.push(el);
        }
        if (el === node) break;
      }
      return out;
    };

    const setOffset = (y: number, animate: boolean) => {
      node.style.transition = animate
        ? `transform ${reduceMotion() ? 0 : 220}ms cubic-bezier(0.32, 0.72, 0, 1)`
        : "none";
      node.style.transform = y === 0 ? "" : `translate3d(0, ${y}px, 0)`;
    };

    const onStart = (e: TouchEvent) => {
      const on = typeof enabledRef.current === "function" ? enabledRef.current() : enabledRef.current;
      if (!on || e.touches.length !== 1) return;
      const target = e.target as Element | null;
      if (target?.closest(NO_SWIPE)) return;
      const t = e.touches[0];
      startX = t.clientX;
      startY = lastY = t.clientY;
      lastT = e.timeStamp;
      velocity = 0;
      fromHandle = t.clientY - node.getBoundingClientRect().top <= HANDLE_ZONE;
      scrollers = findScrollers(target);
      tracking = true;
      dragging = false;
    };

    const onMove = (e: TouchEvent) => {
      if (!tracking) return;
      const t = e.touches[0];
      const dy = t.clientY - startY;
      if (!dragging) {
        const dx = Math.abs(t.clientX - startX);
        if (dy < START_SLOP && dx < START_SLOP) return;
        // 上向き・横向き、またはスクロールの途中から始めた操作はシートを動かさない。
        const atTop = scrollers.every((el) => el.scrollTop <= 0);
        if (dy < START_SLOP || dx > dy || !(fromHandle || atTop)) {
          tracking = false;
          return;
        }
        dragging = true;
      }
      // ここから先はシートを指に追従させる（本文のスクロール・ページの引っ張りを止める）。
      if (e.cancelable) e.preventDefault();
      const dt = Math.max(1, e.timeStamp - lastT);
      velocity = (t.clientY - lastY) / dt;
      lastY = t.clientY;
      lastT = e.timeStamp;
      setOffset(Math.max(0, dy), false);
    };

    const onEnd = (e: TouchEvent) => {
      if (!tracking) return;
      tracking = false;
      if (!dragging) return;
      dragging = false;
      const dy = Math.max(0, lastY - startY);
      const threshold = Math.min(CLOSE_DISTANCE, node.offsetHeight * 0.25);
      // 離す直前まで動いていたときだけ「払った」とみなす（引いて止めてから離したら距離だけで判定）。
      const moving = e.timeStamp - lastT <= FLICK_STALE_MS;
      const flicked = moving && velocity > CLOSE_VELOCITY && dy > START_SLOP * 3;
      if (dy > threshold || flicked) {
        // 位置はそのままにして閉じる（退場アニメーションがこの位置から続く）。
        node.style.transition = "none";
        closeRef.current();
      } else {
        setOffset(0, true);
      }
    };

    // OS・ブラウザによる中断は閉じる操作ではないので、元の位置へ戻すだけにする。
    const onCancel = () => {
      if (!tracking) return;
      tracking = false;
      if (!dragging) return;
      dragging = false;
      setOffset(0, true);
    };

    node.addEventListener("touchstart", onStart, { passive: true });
    node.addEventListener("touchmove", onMove, { passive: false });
    node.addEventListener("touchend", onEnd);
    node.addEventListener("touchcancel", onCancel);
    cleanupRef.current = () => {
      node.removeEventListener("touchstart", onStart);
      node.removeEventListener("touchmove", onMove);
      node.removeEventListener("touchend", onEnd);
      node.removeEventListener("touchcancel", onCancel);
    };
  }, []);
}

/// 呼び出し側の ref とスワイプ用の ref を 1 つにまとめる（毎描画で付け直さないよう memo する）。
export function useMergedRef<T>(a: React.Ref<T> | undefined, b: React.Ref<T> | undefined): React.RefCallback<T> {
  return React.useCallback(
    (node: T | null) => {
      for (const r of [a, b]) {
        if (typeof r === "function") r(node);
        else if (r) (r as React.RefObject<T | null>).current = node;
      }
    },
    [a, b],
  );
}
