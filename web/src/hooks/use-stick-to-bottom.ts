"use client";

import * as React from "react";

/// 下端からこの距離以内にいれば「最新を見ている」とみなす（下へ戻ってきたら追従を再開する）。
const NEAR_BOTTOM_PX = 80;
/// この距離以内なら下端そのもの（小数の scrollTop の端数を吸収する）。
const AT_BOTTOM_PX = 1;
/// 上へ動かす入力のあと、この時間たっても上へ動いていなければ追従へ戻す（横スワイプの指のぶれ・
/// 中の要素だけがスクロールしたホイールなど、会話は動かなかった入力で止めたままにしない）。
const RELEASE_SETTLE_MS = 250;

/// 上へ動かすキー（押した時点で追従をやめる。スクロールが始まる前に止めないと、
/// 次の追従がキーのスクロールを打ち消す）。
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

/// 会話のスクロール追従（チャット専用）。
///
/// - 下端にいる間だけ、中身が伸びたら（トークン・承認カード・計画・図の出現）下端へ合わせる。
///   中身の高さの変化は ResizeObserver で拾うので、何が伸びたかを呼び出し側で列挙しなくてよい。
/// - **生成中は、回答の先頭（`stopAtRef` に渡した要素）が画面の上端に来たらそこで追従を終える。**
///   以降は生成が続いても動かさない（回答を上から読み進められるように）。上端は本文の上の余白
///   （浮いたクロームの下・最初の発話が来る位置）で、「一つ前／一つ後」の移動先と揃える。
/// - 読み返すために上へスクロールしたら追従をやめ、読んでいる位置をそのまま保つ。「最新へ」ボタン
///   （`showJump`）を出す。下へ戻ってきて下端付近に入ったら再び追従する（止まり位置を過ぎた
///   生成中は戻さない）。
/// - 送信したときは `scrollToBottom()`（自分の発話を見せ、次の回答の先頭まで追う）。「最新へ」は
///   `jumpToLatest()`（下端へ一度動かすだけで、生成中は追わない）。
///
/// 「上へ動かした」はスクロール位置の変化だけで判断しない。トラックパッドや指のゆっくりした操作は
/// 1 回の動きが 1〜2px で、位置だけ見ると下端と区別できず、生成中は次のトークンで引き戻していた。
/// 操作の入力（ホイール・指・キー・スクロールバー）の時点で追従をやめ、スクロール位置は
/// 「上へ動いて下端を離れた」ことの確認（入力を伴わない移動・「一つ前」への移動など）に使う。
///
/// `scrollIntoView` は使わない（祖先のスクロール領域＝シェルの main やページ全体まで動かし、
/// スマホでは浮いたクロームごと画面がずれる）。スクロール領域の `scrollTop` だけを動かす。
export function useStickToBottom() {
  const [scroller, setScroller] = React.useState<HTMLDivElement | null>(null);
  const [content, setContent] = React.useState<HTMLDivElement | null>(null);
  const stick = React.useRef(true);
  // 上へ動かす入力があり、まだ下端を離れていない（下端扱いで追従へ戻さない）。
  const leaving = React.useRef(false);
  // 指が触れている・スクロールバーを掴んでいる間は、追従で位置を書き換えない（操作と取り合う）。
  const touchY = React.useRef<number | null>(null);
  const draggingBar = React.useRef(false);
  const lastTop = React.useRef(0);
  const [showJump, setShowJump] = React.useState(false);
  // 生成中の回答の先頭（追従の止まり位置）と、それに着いて追従を終えたか。
  const [stopAt, setStopAt] = React.useState<HTMLElement | null>(null);
  const stopAtEl = React.useRef<HTMLElement | null>(null);
  const stopped = React.useRef(false);

  const distance = (el: HTMLElement) => el.scrollHeight - el.scrollTop - el.clientHeight;

  // 止まり位置の scrollTop（回答の先頭を上端の余白の位置に合わせる値）。生成中でなければ null。
  const stopTop = React.useCallback(
    (el: HTMLElement) => {
      const target = stopAtEl.current;
      if (!target || !content) return null;
      const inset = parseFloat(getComputedStyle(content).paddingTop) || 0;
      return (
        target.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop - inset
      );
    },
    [content],
  );

  // 下端へ合わせる。ただし生成中の回答の先頭が上端に着いたら、そこで止めて追従を終える。
  const pin = React.useCallback(
    (el: HTMLElement) => {
      const limit = stopTop(el);
      if (limit !== null && limit <= el.scrollHeight - el.clientHeight) {
        if (el.scrollTop < limit) el.scrollTop = limit;
        stick.current = false;
        stopped.current = true;
      } else {
        el.scrollTop = el.scrollHeight;
      }
      lastTop.current = el.scrollTop;
    },
    [stopTop],
  );

  // 描画と同時に止まり位置を差し替える（effect を待つと、その間の追従が止まり位置を知らない）。
  const stopAtRef = React.useCallback((el: HTMLElement | null) => {
    stopAtEl.current = el;
    setStopAt(el);
  }, []);

  // 新しい回答が始まった（止まり位置が変わった）。下端付近で待っていたなら、その先頭まで追う。
  React.useEffect(() => {
    stopped.current = false;
    if (stopAt && scroller && distance(scroller) <= NEAR_BOTTOM_PX) stick.current = true;
  }, [stopAt, scroller]);

  React.useEffect(() => {
    if (!scroller) return;
    // 上へ動かす入力: 動き出す前に追従をやめる（追従していて、上へ動ける余地があるときだけ。
    // 読み返し中に中の要素だけが動いた入力で、250ms 後に追従へ戻して下端へ飛ばさない）。
    let settle: ReturnType<typeof setTimeout> | undefined;
    const release = () => {
      const from = scroller.scrollTop;
      if (from <= 0 || !stick.current) return;
      stick.current = false;
      leaving.current = true;
      clearTimeout(settle);
      settle = setTimeout(() => {
        // 上へ動いていれば scroll で `leaving` が落ちている。動かなかった入力なら追従へ戻す。
        if (!leaving.current || scroller.scrollTop < from - AT_BOTTOM_PX) return;
        leaving.current = false;
        stick.current = true;
        if (touchY.current == null && !draggingBar.current) pin(scroller);
      }, RELEASE_SETTLE_MS);
    };
    const onWheel = (e: WheelEvent) => {
      // ctrl ＋ホイールはトラックパッドのピンチ（拡大縮小）でスクロールではない。
      if (e.deltaY < 0 && !e.ctrlKey) release();
    };
    // キーのスクロールは、フォーカスが本文（またはどこにも無い）ときにこの領域へ効く。入力欄の
    // 矢印キーはカーソル移動なので見ない。
    const onKeyDown = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && t !== document.body && !scroller.contains(t)) return;
      if (t?.closest("input, textarea, [contenteditable='true']")) return;
      if (UP_KEYS.has(e.key) || (e.key === " " && e.shiftKey)) release();
    };
    const onTouchStart = (e: TouchEvent) => {
      touchY.current = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY ?? null;
      // 指を下へ動かす＝中身を上へ（読み返す方向）。
      if (y != null && touchY.current != null && y > touchY.current) release();
      touchY.current = y;
    };
    const onTouchEnd = (e: TouchEvent) => {
      if (e.touches.length > 0) return;
      touchY.current = null;
      if (stick.current) pin(scroller);
    };
    // スクロールバーを掴んだ（左右の余白もスクロール領域そのものなので、位置で見分ける）。
    // 掴んだだけでは追従をやめない。上へ動かせば scroll で止まる。
    const onPointerDown = (e: PointerEvent) => {
      if (e.pointerType !== "mouse" || e.target !== scroller) return;
      if (e.offsetX >= scroller.clientWidth) draggingBar.current = true;
    };
    // 指を離した。touchend は触れ始めた要素へ届くので、その要素が生成の確定で作り直されて
    // 外れていると、スクロール領域まで上がってこない。pointerup は指の下の要素へ届く。
    const onPointerUp = (e: PointerEvent) => {
      draggingBar.current = false;
      if (e.pointerType !== "touch" || touchY.current == null) return;
      touchY.current = null;
      if (stick.current) pin(scroller);
    };
    const onScroll = () => {
      const top = scroller.scrollTop;
      const d = distance(scroller);
      const delta = top - lastTop.current;
      lastTop.current = top;
      if (delta < 0 && d > AT_BOTTOM_PX) {
        // 上へ動いて下端を離れた（入力を伴わない移動も含む）。
        stick.current = false;
        leaving.current = false;
      } else if (stopped.current) {
        // 生成中の回答の先頭まで追い終えた。下端へ戻ってきても、生成が終わるまでは追わない。
      } else if (d <= AT_BOTTOM_PX) {
        // 下端（中身が縮んで押し戻された場合も含む）。上へ動かす入力の直後は、まだ動き出して
        // いないだけなので追従へ戻さない（下へ動いて着いたのなら戻す）。
        if (delta > 0) leaving.current = false;
        if (!leaving.current) stick.current = true;
      } else if (delta > 0 && d <= NEAR_BOTTOM_PX) {
        // 下へ戻ってきて下端付近に入った。
        stick.current = true;
        leaving.current = false;
      }
      setShowJump(d > NEAR_BOTTOM_PX);
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    scroller.addEventListener("wheel", onWheel, { passive: true });
    document.addEventListener("keydown", onKeyDown);
    scroller.addEventListener("touchstart", onTouchStart, { passive: true });
    scroller.addEventListener("touchmove", onTouchMove, { passive: true });
    scroller.addEventListener("touchend", onTouchEnd, { passive: true });
    scroller.addEventListener("touchcancel", onTouchEnd, { passive: true });
    scroller.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    return () => {
      clearTimeout(settle);
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("wheel", onWheel);
      document.removeEventListener("keydown", onKeyDown);
      scroller.removeEventListener("touchstart", onTouchStart);
      scroller.removeEventListener("touchmove", onTouchMove);
      scroller.removeEventListener("touchend", onTouchEnd);
      scroller.removeEventListener("touchcancel", onTouchEnd);
      scroller.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
    };
  }, [scroller, pin]);

  React.useEffect(() => {
    if (!scroller || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stick.current && touchY.current == null && !draggingBar.current) pin(scroller);
      setShowJump(distance(scroller) > NEAR_BOTTOM_PX);
    });
    // 中身（伸びる本文）と、スクロール領域そのもの（キーボードが出た）の両方。本文は padding も
    // 含めて見る（重ねた入力欄が伸びると、本文の下の padding が伸びる）。
    ro.observe(scroller);
    if (content) ro.observe(content, { box: "border-box" });
    return () => ro.disconnect();
  }, [scroller, content, pin]);

  // 送信した: 自分の発話を見せ、次の回答の先頭まで追う。
  const scrollToBottom = React.useCallback(() => {
    stick.current = true;
    stopped.current = false;
    leaving.current = false;
    if (scroller) pin(scroller);
  }, [scroller, pin]);

  // 「最新へ」: 下端へ一度動かす。生成中は追わない（止まり位置を過ぎたものとして扱う）。
  const jumpToLatest = React.useCallback(() => {
    leaving.current = false;
    if (stopAtEl.current) {
      stick.current = false;
      stopped.current = true;
    } else {
      stick.current = true;
    }
    const el = scroller;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    // 滑らかに動く間にも本文は伸びるので、押した時点の下端に着いても最新には届かない。動き終わったら
    // その時点の下端へ合わせ直す（途中で利用者が操作したら、その操作を優先して何もしない）。
    const inputs = ["wheel", "touchstart", "keydown", "pointerdown"] as const;
    let done = false;
    const finish = (snap: boolean) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      el.removeEventListener("scrollend", onEnd);
      for (const ev of inputs) el.removeEventListener(ev, onInput);
      if (snap) el.scrollTop = el.scrollHeight;
    };
    const onEnd = () => finish(true);
    const onInput = () => finish(false);
    el.addEventListener("scrollend", onEnd);
    for (const ev of inputs) el.addEventListener(ev, onInput, { passive: true });
    // scrollend の無いブラウザ（Safari の一部）向けの保険。
    const timer = setTimeout(onEnd, 1_000);
  }, [scroller]);

  return {
    scrollerRef: setScroller,
    contentRef: setContent,
    stopAtRef,
    scroller,
    content,
    showJump,
    scrollToBottom,
    jumpToLatest,
  };
}
