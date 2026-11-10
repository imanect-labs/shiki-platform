"use client";

import * as React from "react";

import { readThreadScroll, writeThreadScroll } from "@/lib/thread-scroll";

/// 会話の行（メッセージ・生成中の回答）に付ける目印。値は保存済みの回答 ID（生成中の回答は生成先の
/// ID）なので、生成中に覚えた位置も確定後の行で見つかる。
export const ANCHOR_ATTR = "data-anchor";

/// 下端からこの距離以内なら「最新を見ていた」とみなす（追従の判定と揃える）。
const NEAR_BOTTOM_PX = 80;
/// スクロール中の保存の間引き。
const SAVE_EVERY_MS = 400;

/// 会話ごとのスクロール位置を覚え、戻ったときにそこから表示する（#1154）。
///
/// 戻り方は、離れたときの状態で変える（自動スクロールとぶつからないように）:
/// - 上で読んでいた → 覚えた位置を復元する（生成中でも追わない。増えた分は「最新へ」で辿る）。
/// - 最新を見ていた・まだ生成中 → 最新の位置を追い続ける（#1152 の `followLatest` のまま）。
/// - 最新を見ていた・離れている間に回答が増えた → 増えた分の先頭を上端に合わせる（下端へ飛ぶと
///   回答の途中から読むことになる。生成中に回答の先頭で止まるのと揃える）。
/// - 最新を見ていた・変化なし、または記録なし → 今までどおり（下端）。
/// URL に行き先（`#...`）があるときは、その行き先を優先して戻さない。
///
/// 返す関数は、会話を読み込んで描画した直後に 1 度だけ呼ぶ（同じ会話の読み直しでは戻さない）。
export function useScrollMemory({
  threadId,
  scroller,
  content,
  holdAt,
}: {
  threadId: string;
  scroller: HTMLElement | null;
  content: HTMLElement | null;
  holdAt: (getTop: () => number | null) => void;
}): (opts: { resuming: boolean }) => void {
  // 戻すかどうかを決め終えるまでは保存しない（読み込み直後の下端の位置で記録を上書きしない）。
  const decided = React.useRef<string | null>(null);

  const anchors = React.useCallback(
    () => (content ? Array.from(content.querySelectorAll<HTMLElement>(`[${ANCHOR_ATTR}]`)) : []),
    [content],
  );
  const inset = React.useCallback(
    () => (content ? parseFloat(getComputedStyle(content).paddingTop) || 0 : 0),
    [content],
  );
  // 行の上端を、本文の上の余白の位置に合わせる scrollTop。
  const topOf = React.useCallback(
    (el: HTMLElement) => {
      if (!scroller) return 0;
      return (
        scroller.scrollTop + el.getBoundingClientRect().top - scroller.getBoundingClientRect().top - inset()
      );
    },
    [scroller, inset],
  );
  const find = React.useCallback(
    (anchor: string) => anchors().find((el) => el.getAttribute(ANCHOR_ATTR) === anchor) ?? null,
    [anchors],
  );

  // いまの位置を記録に写す（書き込みは間引く）。会話を切り替えた後始末の時点ではもう次の会話が
  // 描画されているので、書き込むのは「その会話を表示していた間に」写した記録だけにする。
  const pending = React.useRef<{ thread: string; rec: Parameters<typeof writeThreadScroll>[1] } | null>(null);
  const capture = React.useCallback(() => {
    if (!scroller || decided.current !== threadId) return;
    const rows = anchors();
    if (rows.length === 0) return;
    const line = scroller.getBoundingClientRect().top + inset();
    const top = rows.find((el) => el.getBoundingClientRect().bottom > line) ?? rows[rows.length - 1];
    pending.current = {
      thread: threadId,
      rec: {
        anchor: top.getAttribute(ANCHOR_ATTR),
        offset: Math.round(line - top.getBoundingClientRect().top),
        atBottom: scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= NEAR_BOTTOM_PX,
        last: rows[rows.length - 1].getAttribute(ANCHOR_ATTR),
      },
    };
  }, [scroller, threadId, anchors, inset]);
  const flush = React.useCallback(() => {
    const p = pending.current;
    pending.current = null;
    if (p) writeThreadScroll(p.thread, p.rec);
  }, []);

  // スクロールのたびに写し（書き込みは間引く）、タブが裏へ回るとき・ページを離れるとき・会話を離れるときに書く。
  React.useEffect(() => {
    if (!scroller) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onScroll = () => {
      capture();
      if (timer) return;
      timer = setTimeout(() => {
        timer = undefined;
        flush();
      }, SAVE_EVERY_MS);
    };
    const onHide = () => {
      if (document.visibilityState !== "hidden") return;
      capture();
      flush();
    };
    const onPageHide = () => {
      capture();
      flush();
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    document.addEventListener("visibilitychange", onHide);
    window.addEventListener("pagehide", onPageHide);
    return () => {
      clearTimeout(timer);
      flush();
      scroller.removeEventListener("scroll", onScroll);
      document.removeEventListener("visibilitychange", onHide);
      window.removeEventListener("pagehide", onPageHide);
    };
  }, [scroller, capture, flush]);

  const decide = React.useCallback(
    (resuming: boolean) => {
      const rec = readThreadScroll(threadId);
      decided.current = threadId;
      if (!rec || window.location.hash) return;
      if (rec.atBottom) {
        if (resuming || !rec.last) return; // 生成中は最新を追う／記録が古い形
        const rows = anchors();
        const at = rows.findIndex((el) => el.getAttribute(ANCHOR_ATTR) === rec.last);
        const next = at >= 0 ? rows[at + 1] : undefined;
        if (!next) return; // 増えた分が無い（または前の行が見つからない）→ 下端のまま
        const anchor = next.getAttribute(ANCHOR_ATTR) ?? "";
        holdAt(() => {
          const el = find(anchor);
          return el ? topOf(el) : null;
        });
        return;
      }
      if (!rec.anchor || !find(rec.anchor)) return;
      const anchor = rec.anchor;
      holdAt(() => {
        const el = find(anchor);
        return el ? topOf(el) + rec.offset : null;
      });
    },
    [threadId, anchors, find, holdAt, topOf],
  );

  return React.useCallback(
    ({ resuming }: { resuming: boolean }) => {
      if (decided.current === threadId) return;
      // 読み込んだ行が描画されてから決める（setState の直後はまだ DOM に無い）。決めた後の位置も
      // 写しておく（一度もスクロールせずに離れても、最新を見ていたことを覚える）。
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          decide(resuming);
          requestAnimationFrame(capture);
        }),
      );
    },
    [threadId, decide, capture],
  );
}
