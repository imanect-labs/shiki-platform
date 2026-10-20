"use client";

import * as React from "react";

/// 下端からこの距離以内にいれば「最新を見ている」とみなして追従する。
const NEAR_BOTTOM_PX = 80;

/// 会話のスクロール追従（チャット専用）。
///
/// - 下端付近にいる間だけ、中身が伸びたら（トークン・承認カード・計画・図の出現）下端へ合わせる。
///   中身の高さの変化は ResizeObserver で拾うので、何が伸びたかを呼び出し側で列挙しなくてよい。
/// - 読み返すために上へスクロールしたら追従をやめ、「最新へ」ボタン（`showJump`）を出す。
/// - 送信したときなど、呼び出し側が明示的に下端へ戻すときは `scrollToBottom()`。
///
/// `scrollIntoView` は使わない（祖先のスクロール領域＝シェルの main やページ全体まで動かし、
/// スマホでは浮いたクロームごと画面がずれる）。スクロール領域の `scrollTop` だけを動かす。
export function useStickToBottom() {
  const [scroller, setScroller] = React.useState<HTMLDivElement | null>(null);
  const [content, setContent] = React.useState<HTMLDivElement | null>(null);
  const stick = React.useRef(true);
  const lastTop = React.useRef(0);
  const [showJump, setShowJump] = React.useState(false);

  const distance = (el: HTMLElement) => el.scrollHeight - el.scrollTop - el.clientHeight;

  const pin = React.useCallback((el: HTMLElement) => {
    el.scrollTop = el.scrollHeight;
    lastTop.current = el.scrollTop;
  }, []);

  React.useEffect(() => {
    if (!scroller) return;
    const onScroll = () => {
      const top = scroller.scrollTop;
      const d = distance(scroller);
      const movedUp = top < lastTop.current - 2;
      lastTop.current = top;
      // ぴったり下端（中身が縮んで押し戻された場合も含む）なら追従。上へ動かしたら（指・ホイール・
      // キーボード）追従をやめる。下へ戻ってきて下端付近に入ったら再び追従する。
      if (d <= 2) stick.current = true;
      else if (movedUp) stick.current = false;
      else if (d <= NEAR_BOTTOM_PX) stick.current = true;
      setShowJump(d > NEAR_BOTTOM_PX);
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    return () => scroller.removeEventListener("scroll", onScroll);
  }, [scroller]);

  React.useEffect(() => {
    if (!scroller || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stick.current) pin(scroller);
      setShowJump(distance(scroller) > NEAR_BOTTOM_PX);
    });
    // 中身（伸びる本文）と、スクロール領域そのもの（キーボードが出た）の両方。本文は padding も
    // 含めて見る（重ねた入力欄が伸びると、本文の下の padding が伸びる）。
    ro.observe(scroller);
    if (content) ro.observe(content, { box: "border-box" });
    return () => ro.disconnect();
  }, [scroller, content, pin]);

  const scrollToBottom = React.useCallback(
    (behavior: ScrollBehavior = "auto") => {
      stick.current = true;
      if (!scroller) return;
      if (behavior === "smooth") {
        scroller.scrollTo({ top: scroller.scrollHeight, behavior });
      } else {
        pin(scroller);
      }
    },
    [scroller, pin],
  );

  return { scrollerRef: setScroller, contentRef: setContent, showJump, scrollToBottom };
}
