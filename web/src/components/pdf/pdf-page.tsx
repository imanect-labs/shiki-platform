"use client";

/// PDF の 1 ページを描き、引用箇所の枠と（任意で）テキスト層・検索の一致を重ねる（#508）。
///
/// 枠は Docling の prov（PDF ポイント座標）。ページの viewport で表示座標へ写すので、回転にも追従する。
///
/// 大きな PDF でも重くしないよう、**画面に近いページだけ**を描く。画面から離れたページは
/// canvas とテキスト層を空にしてメモリを返す（高 DPI では 1 ページ十数 MB になる）。寸法も近づいてから
/// 取り、それまでは `estimate`（1 ページ目の縦横比から出した高さ）で場所だけ確保する。
///
/// テキスト層は pdf.js の `TextLayer`（透明な文字を canvas に重ね、選択・コピー・検索の塗りに使う）。
/// 文字は `pageTextContent` のキャッシュを検索と共有する（span の並びと検索の文字位置を一致させる）。
import * as React from "react";
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";

import { cn } from "@/lib/utils";
import { boxToViewport, pdfjsLib } from "@/lib/pdf";
import { setPageHighlights } from "@/lib/pdf-highlight";
import { buildPageText, matchPieces, pageTextContent, type PageText } from "@/lib/pdf-text";
import type { CiteBox } from "@/lib/citation-locate";

type Size = { w: number; h: number; viewport: PageViewport };

/// 1 ページの canvas の画素数の上限。
const MAX_CANVAS_PIXELS = 16_000_000;

/// このページの検索の一致（ページ文字列上の [開始, 終了)）。`active` は今の一致。
export type PageHighlights = { ranges: ReadonlyArray<readonly [number, number]>; active: readonly [number, number] | null };

export function PdfPage({
  doc,
  pageNumber,
  width,
  estimate,
  boxes = [],
  className,
  eager = false,
  rotation = 0,
  textLayer = false,
  highlights,
}: {
  doc: PDFDocumentProxy;
  pageNumber: number;
  /// 表示幅（CSS px）。高さはページの縦横比から決まる。
  width: number;
  /// 寸法を取るまでの仮の高さ（既定は A4 の縦横比）。
  estimate?: number;
  boxes?: readonly CiteBox[];
  className?: string;
  /// 画面外でもすぐ描く（出典パネルの縮小表示・ジャンプ先のページ）。
  eager?: boolean;
  /// 表示の回転（0/90/180/270 度・時計回り）。
  rotation?: number;
  /// 文字を選択・コピーできるようにする（ビューア本体のみ。サムネイルや出典の縮小表示では出さない）。
  textLayer?: boolean;
  highlights?: PageHighlights;
}) {
  const wrapRef = React.useRef<HTMLDivElement>(null);
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const textRef = React.useRef<HTMLDivElement>(null);
  const [size, setSize] = React.useState<Size | null>(null);
  const [near, setNear] = React.useState(eager);
  // テキスト層の描画結果（span と、検索と同じ文字位置）。描き直すたびに差し替わる。
  const [text, setText] = React.useState<{ divs: HTMLElement[]; page: PageText } | null>(null);
  const highlightKey = React.useId();

  // 画面に近づいた / 離れたを追う（離れたら canvas を空にする）。
  React.useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const io = new IntersectionObserver((entries) => setNear(eager || entries.some((e) => e.isIntersecting)), {
      rootMargin: "1200px 0px",
    });
    io.observe(el);
    return () => io.disconnect();
  }, [eager]);

  // 寸法は近づいてから取る（全ページぶんの getPage を開いた瞬間に走らせない）。
  React.useEffect(() => {
    if (!near) return;
    let active = true;
    doc
      .getPage(pageNumber)
      .then((page) => {
        if (!active) return;
        const base = page.getViewport({ scale: 1, rotation });
        const viewport = page.getViewport({ scale: width / base.width, rotation });
        setSize({ w: width, h: viewport.height, viewport });
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [doc, pageNumber, width, near, rotation]);

  React.useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    if (!near || !size) {
      // 画面から離れたページは画素を手放す。
      canvas.width = 0;
      canvas.height = 0;
      return;
    }
    let task: { cancel: () => void; promise: Promise<void> } | null = null;
    let active = true;
    doc
      .getPage(pageNumber)
      .then((page) => {
        if (!active) return;
        // 拡大しても canvas の画素数を上限内に収める（iOS Safari は約 16M 画素を超えると描けない）。
        const cap = Math.sqrt(MAX_CANVAS_PIXELS / Math.max(1, size.viewport.width * size.viewport.height));
        const dpr = Math.min(window.devicePixelRatio || 1, 2, cap);
        const viewport = size.viewport.clone({ scale: size.viewport.scale * dpr });
        canvas.width = Math.floor(viewport.width);
        canvas.height = Math.floor(viewport.height);
        const ctx = canvas.getContext("2d");
        if (!ctx) return;
        task = page.render({ canvasContext: ctx, viewport });
        task.promise.catch(() => undefined);
      })
      .catch(() => undefined);
    return () => {
      active = false;
      task?.cancel();
    };
  }, [doc, pageNumber, size, near]);

  // テキスト層（近いページだけ）。寸法・回転が変わったら描き直す。
  React.useEffect(() => {
    const container = textRef.current;
    if (!container || !textLayer || !near || !size) return;
    let active = true;
    let layer: { cancel: () => void } | null = null;
    Promise.all([pdfjsLib(), pageTextContent(doc, pageNumber)])
      .then(async ([lib, content]) => {
        if (!active) return;
        container.replaceChildren();
        const tl = new lib.TextLayer({ textContentSource: content, container, viewport: size.viewport });
        layer = tl;
        await tl.render();
        if (active) setText({ divs: tl.textDivs, page: buildPageText(content) });
      })
      .catch(() => undefined);
    return () => {
      active = false;
      layer?.cancel();
      container.replaceChildren();
      setText(null);
    };
  }, [doc, pageNumber, size, near, textLayer]);

  // 検索の一致を塗る。今の一致はページ内で見える位置までスクロールする。
  const activeKey = highlights?.active ? highlights.active.join(":") : "";
  const scrolledTo = React.useRef("");
  React.useEffect(() => {
    if (!text || !highlights || (highlights.ranges.length === 0 && !highlights.active)) {
      setPageHighlights(highlightKey, [], null);
      return;
    }
    const toRange = ([s, e]: readonly [number, number]): Range | null => {
      const pieces = matchPieces(text.page.segments, s, e);
      if (pieces.length === 0) return null;
      const [fi, fa] = pieces[0];
      const [li, , lb] = pieces[pieces.length - 1];
      const first = text.divs[fi]?.firstChild;
      const last = text.divs[li]?.firstChild;
      if (!first || !last) return null;
      const r = document.createRange();
      r.setStart(first, Math.min(fa, first.textContent?.length ?? 0));
      r.setEnd(last, Math.min(lb, last.textContent?.length ?? 0));
      return r;
    };
    const all = highlights.ranges.map(toRange).filter((r): r is Range => r != null);
    const active = highlights.active ? toRange(highlights.active) : null;
    setPageHighlights(highlightKey, all, active);
    if (active && scrolledTo.current !== activeKey) {
      scrolledTo.current = activeKey;
      (active.startContainer.parentElement ?? null)?.scrollIntoView({ block: "center", inline: "nearest" });
    }
    if (!active) scrolledTo.current = "";
  }, [text, highlights, highlightKey, activeKey]);
  React.useEffect(() => () => setPageHighlights(highlightKey, [], null), [highlightKey]);

  const mine = boxes.filter((b) => b.page === pageNumber);
  const height = size?.h ?? estimate ?? width * 1.414;
  return (
    <div
      ref={wrapRef}
      data-page={pageNumber}
      className={cn("relative overflow-hidden rounded-sm bg-white shadow-sm ring-1 ring-black/[0.06]", className)}
      style={{ width, height, ["--scale-factor" as string]: size?.viewport.scale ?? 1 }}
    >
      <canvas ref={canvasRef} className="absolute inset-0 h-full w-full" aria-hidden />
      {textLayer ? <div ref={textRef} className="pdf-text-layer" /> : null}
      {size
        ? mine.map((b, i) => {
            const css = boxToViewport(b.bbox, b.topLeft, size.viewport);
            return (
              <div
                key={i}
                data-cite-box=""
                className="pointer-events-none absolute z-[2] rounded-[3px] bg-[var(--season-autumn)]/15 ring-2 ring-[var(--season-autumn)]/70"
                style={{ left: css.left - 3, top: css.top - 3, width: css.width + 6, height: css.height + 6 }}
              />
            );
          })
        : null}
    </div>
  );
}
