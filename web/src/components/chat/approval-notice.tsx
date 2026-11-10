"use client";

import * as React from "react";

import { ArrowDown, ShieldAlert } from "lucide-react";

import type { ApprovalRequest } from "@/lib/chat-api";
import { describeTool } from "@/lib/tool-display";

/// 承認カードがこれだけ見えていれば「見えている」とみなす（カードの高さ比 or 表示領域の高さ比）。
const VISIBLE_RATIO = 0.5;
/// 交差の比率がこの段を越えるたびに判定し直す（0.05 刻み）。
const THRESHOLDS = Array.from({ length: 21 }, (_, i) => i / 20);
/// 「確認する」でカードを出すとき、カードの下端と入力欄の間に空ける余白。
const REVEAL_GAP_PX = 16;
/// 承認待ちの間、タブのタイトルに付ける印。
const TITLE_MARK = "（承認待ち）";

/// 承認カードが会話の表示領域（重ねた入力欄の下は除く）の外にあるか。
///
/// 生成中は回答の先頭で追従を止めるので、承認カードは画面の下に出て気付けないことがある。
/// 位置を勝手に動かさず、外にあるときだけ入力欄の上に帯を出すために使う。
export function useOffscreen(
  scroller: HTMLElement | null,
  target: HTMLElement | null,
  bottomInset: number,
): boolean {
  // 判定はどのカードについてのものかと組で持つ。入力欄の高さが変わって observer を作り直しても、
  // 同じカードなら直前の判定を使い続ける（作り直しのたびに帯が消えてちらつかないように）。
  // 新しいカードは最初の判定が届くまで「見えている」扱い。
  const [state, setState] = React.useState<{ el: HTMLElement; offscreen: boolean } | null>(null);
  React.useEffect(() => {
    if (!scroller || !target || typeof IntersectionObserver === "undefined") return;
    const io = new IntersectionObserver(
      ([e]) => {
        if (!e) return;
        const rootHeight = e.rootBounds?.height ?? 0;
        const seen =
          e.isIntersecting &&
          (e.intersectionRatio >= VISIBLE_RATIO ||
            (rootHeight > 0 && e.intersectionRect.height >= rootHeight * VISIBLE_RATIO));
        setState((prev) =>
          prev?.el === target && prev.offscreen === !seen ? prev : { el: target, offscreen: !seen },
        );
      },
      {
        root: scroller,
        // 入力欄は本文の上に重なっているので、その高さぶんは見えていない領域として扱う。
        rootMargin: `0px 0px -${Math.max(0, Math.round(bottomInset))}px 0px`,
        // 表示領域より高いカードは見えている高さで判定するので、比率の段を細かく取る。
        threshold: THRESHOLDS,
      },
    );
    io.observe(target);
    return () => io.disconnect();
  }, [scroller, target, bottomInset]);
  return state !== null && state.el === target && state.offscreen;
}

/// 承認待ちの間、タブのタイトルの先頭に印を付ける（別のタブにいても気付けるように）。
/// 外すときは自分が付けた印だけを外す（その間にページがタイトルを変えていても壊さない）。
export function useApprovalTitle(active: boolean) {
  React.useEffect(() => {
    if (!active) return;
    if (!document.title.startsWith(TITLE_MARK)) document.title = `${TITLE_MARK}${document.title}`;
    // Next.js が遷移やメタデータの更新でタイトルを書き換えたら、印を付け直す。
    const head = document.querySelector("head");
    const mo =
      head && typeof MutationObserver !== "undefined"
        ? new MutationObserver(() => {
            if (!document.title.startsWith(TITLE_MARK)) document.title = `${TITLE_MARK}${document.title}`;
          })
        : null;
    if (head) mo?.observe(head, { subtree: true, childList: true, characterData: true });
    return () => {
      mo?.disconnect();
      if (document.title.startsWith(TITLE_MARK)) document.title = document.title.slice(TITLE_MARK.length);
    };
  }, [active]);
}

/// 承認カードが見える位置まで会話をスクロールし、カードへフォーカスを移す。
/// `scrollIntoView` は祖先のスクロール領域まで動かすので使わない（use-stick-to-bottom と同じ）。
export function revealApprovalCard(scroller: HTMLElement, card: HTMLElement, bottomInset: number) {
  const box = scroller.getBoundingClientRect();
  const r = card.getBoundingClientRect();
  const visibleBottom = box.height - bottomInset - REVEAL_GAP_PX;
  const cardTop = r.top - box.top + scroller.scrollTop;
  // カードの下端（承認・却下のボタン）が入力欄のすぐ上に来る位置。カードが表示領域より高ければ
  // 先頭を上端に合わせる（何を承認するのかを上から読めるように）。
  const top = r.height <= visibleBottom ? cardTop + r.height - visibleBottom : cardTop - REVEAL_GAP_PX;
  const max = scroller.scrollHeight - scroller.clientHeight;
  const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  scroller.scrollTo({ top: Math.min(max, Math.max(0, top)), behavior: reduce ? "auto" : "smooth" });
  card.focus({ preventScroll: true });
}

/// 「承認待ち」の帯。入力欄の上に浮かせる。ここでは承認させない（内容を見ずに承認させないため、
/// 「確認する」で承認カードへ連れて行くだけにする）。
export function ApprovalBanner({ request, onReveal }: { request: ApprovalRequest; onReveal: () => void }) {
  const d = describeTool(request);
  const what = `${d.lead}${d.verb}`;
  return (
    <div
      data-testid="approval-banner"
      className="flex items-center gap-2 rounded-full border border-primary/40 bg-background/95 py-1 pl-3 pr-1 text-[13px] shadow-md backdrop-blur"
    >
      <ShieldAlert className="size-4 shrink-0 text-primary" aria-hidden />
      <span className="min-w-0 truncate">
        <span className="font-semibold text-foreground">承認が必要です</span>
        <span className="text-foreground/70 max-md:hidden">：{what}</span>
      </span>
      <button
        type="button"
        onClick={onReveal}
        data-testid="approval-banner-reveal"
        className="inline-flex h-8 shrink-0 items-center gap-1 rounded-full bg-primary px-3 text-[13px] font-medium text-primary-foreground transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
      >
        確認する
        <ArrowDown className="size-3.5" aria-hidden />
      </button>
    </div>
  );
}

/// 承認待ちになったことをスクリーンリーダーへ知らせる（常に置いておく live region）。
/// 帯は画面外のときだけ出るので、帯そのものを live region にすると読み上げられないことがある。
export function ApprovalAnnouncer({ request }: { request: ApprovalRequest | null }) {
  let text = "";
  if (request) {
    const d = describeTool(request);
    text = `承認が必要です：${d.lead}${d.verb}`;
  }
  return (
    <div role="status" aria-live="polite" className="sr-only">
      {text}
    </div>
  );
}
