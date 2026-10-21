"use client";

import * as React from "react";

/// 長押しとみなすまでの時間（ms）と、指が動いたら長押しを取り消す距離（px）。
const LONG_PRESS_MS = 500;
const MOVE_TOLERANCE_PX = 10;

/// タッチの長押しで `onLongPress` を呼ぶ（iOS Safari はリンクの長押しで contextmenu を出さない
/// ため、右クリック・Android の contextmenu とは別にタイマーで拾う）。
///
/// 長押しの後に続けて来る click は `consumeClick()` で握りつぶす（リンクへ遷移させない）。
/// 指が動いたら（スクロール・スワイプ）取り消す。
export function useLongPress(onLongPress: () => void) {
  const timer = React.useRef<number | null>(null);
  const origin = React.useRef<{ x: number; y: number } | null>(null);
  const fired = React.useRef(false);
  const callback = React.useRef(onLongPress);
  callback.current = onLongPress;

  const cancel = React.useCallback(() => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = null;
    origin.current = null;
  }, []);

  React.useEffect(() => cancel, [cancel]);

  const handlers = React.useMemo(
    () => ({
      onTouchStart: (e: React.TouchEvent) => {
        fired.current = false;
        cancel();
        if (e.touches.length !== 1) return;
        const t = e.touches[0];
        origin.current = { x: t.clientX, y: t.clientY };
        timer.current = window.setTimeout(() => {
          timer.current = null;
          fired.current = true;
          callback.current();
        }, LONG_PRESS_MS);
      },
      onTouchMove: (e: React.TouchEvent) => {
        const o = origin.current;
        const t = e.touches[0];
        if (!o || !t) return;
        if (
          Math.abs(t.clientX - o.x) > MOVE_TOLERANCE_PX ||
          Math.abs(t.clientY - o.y) > MOVE_TOLERANCE_PX
        ) {
          cancel();
        }
      },
      onTouchEnd: cancel,
      onTouchCancel: cancel,
    }),
    [cancel],
  );

  /// 直前の操作が長押しだったら true を返し、状態を戻す（click ハンドラの先頭で呼ぶ）。
  const consumeClick = React.useCallback(() => {
    const was = fired.current;
    fired.current = false;
    return was;
  }, []);

  return { handlers, consumeClick };
}
