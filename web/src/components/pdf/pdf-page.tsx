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
/// 検索の文字位置とnative spanを明示対応させる。行座標の無いOCRは段落・表の枠で示す。
import * as React from "react";
import {
  measureFineUnits,
  type FineUnit,
  type FineContent,
  type SelectionPrecision,
} from "@/lib/pdf-fine-selection";
import { pdfBlocks } from "@/lib/pdf-excerpts";
import { PdfRegionLayer } from "@/components/pdf/pdf-region-layer";
import {
  syntheticOcrRect,
  nativePdfText,
  nativeTextOrigin,
  type PdfExcerpt,
  type PdfRect,
} from "@/lib/pdf-excerpts";
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";

import { cn } from "@/lib/utils";
import { boxToViewport, pdfjsLib } from "@/lib/pdf";
import { setPageHighlights } from "@/lib/pdf-highlight";
import {
  buildPageText,
  matchPieces,
  pageTextContent,
  type PageText,
} from "@/lib/pdf-text";
import type { CiteBox } from "@/lib/citation-locate";

type Size = { w: number; h: number; viewport: PageViewport; rotation: number };

/// 1 ページの canvas の画素数の上限。
const MAX_CANVAS_PIXELS = 16_000_000;

/// このページの検索の一致（ページ文字列上の [開始, 終了)）。`active` は今の一致。
export type PageHighlights = {
  ranges: ReadonlyArray<readonly [number, number]>;
  active: readonly [number, number] | null;
};

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
  regionSelection,
  textRevision = 0,
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
  textRevision?: number;
  regionSelection?: {
    enabled: boolean;
    selections: PdfExcerpt[];
    precision?: SelectionPrecision;
    onSelect: (
      page: number,
      bbox: PdfRect,
      replaceId?: string,
      fine?: FineContent,
    ) => void;
  };
}) {
  const wrapRef = React.useRef<HTMLDivElement>(null);
  const fineRequired =
    regionSelection?.enabled &&
    (regionSelection.precision === "line" ||
      regionSelection.precision === "cell" ||
      regionSelection.selections.some(
        (s) => s.page === pageNumber && s.precision,
      ));
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const textRef = React.useRef<HTMLDivElement>(null);
  const [size, setSize] = React.useState<Size | null>(null);
  // ページの縦横比（その回転での 高さ / 幅）。倍率・回転を変えた瞬間にも高さを同じ描画で決めるために持つ
  // （寸法の取り直しは非同期なので、待つと呼び出し側が見ている場所を保てない）。
  const [ratio, setRatio] = React.useState<{
    rotation: number;
    value: number;
  } | null>(null);
  const [near, setNear] = React.useState(eager);
  // テキスト層の描画結果（span と、検索と同じ文字位置）。描き直すたびに差し替わる。
  const [text, setText] = React.useState<{
    divs: (HTMLElement | null)[];
    page: PageText;
    ocr: Map<number, PdfRect>;
  } | null>(null);
  const highlightKey = React.useId();
  const [fineUnits, setFineUnits] = React.useState<{
    viewport: PageViewport;
    values: FineUnit[];
  } | null>(null);

  // 画面に近づいた / 離れたを追う（離れたら canvas を空にする）。
  React.useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => setNear(eager || entries.some((e) => e.isIntersecting)),
      {
        rootMargin: "1200px 0px",
      },
    );
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
        // 表示の回転は、PDF がページに持つ回転（/Rotate）に足す（上書きすると元から横向きの文書が崩れる）。
        const total = (page.rotate + rotation) % 360;
        const base = page.getViewport({ scale: 1, rotation: total });
        const viewport = page.getViewport({
          scale: width / base.width,
          rotation: total,
        });
        setSize({ w: width, h: viewport.height, viewport, rotation });
        setRatio({ rotation, value: base.height / base.width });
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
        const cap = Math.sqrt(
          MAX_CANVAS_PIXELS /
            Math.max(1, size.viewport.width * size.viewport.height),
        );
        const dpr = Math.min(window.devicePixelRatio || 1, 2, cap);
        const viewport = size.viewport.clone({
          scale: size.viewport.scale * dpr,
        });
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
    Promise.all([
      pdfjsLib(),
      pageTextContent(doc, pageNumber),
      nativePdfText(doc, pageNumber),
      pdfBlocks(doc, pageNumber).catch(() => []),
    ])
      .then(async ([lib, content, native, blocks]) => {
        if (!active) return;
        container.replaceChildren();
        const tl = new lib.TextLayer({
          textContentSource: native,
          container,
          viewport: size.viewport,
        });
        layer = tl;
        await tl.render();
        if (active) {
          const ocr = new Map<number, PdfRect>();
          const divByItem = new Map(
            native.items
              .filter((item) => "str" in item)
              .map((item, index) => [item, tl.textDivs[index] ?? null]),
          );
          const divs = content.items
            .filter((item) => "str" in item)
            .map((item, index) => {
              const rect = syntheticOcrRect(item);
              if (rect) {
                ocr.set(index, rect);
                return null;
              }
              return divByItem.get(nativeTextOrigin(item)) ?? null;
            });
          setText({ divs, page: buildPageText(content), ocr });
          if (wrapRef.current && fineRequired)
            setFineUnits({
              viewport: size.viewport,
              values: measureFineUnits(
                wrapRef.current,
                tl.textDivs,
                native,
                size.viewport,
                blocks,
              ),
            });
        }
      })
      .catch(() => undefined);
    return () => {
      active = false;
      layer?.cancel();
      container.replaceChildren();
      setText(null);
      setFineUnits(null);
    };
  }, [doc, pageNumber, size, near, textLayer, textRevision, fineRequired]);

  // 検索の一致を塗る。今の一致はページ内で見える位置までスクロールする。
  const activeKey = highlights?.active ? highlights.active.join(":") : "";
  const scrolledTo = React.useRef("");
  React.useEffect(() => {
    if (
      !text ||
      !highlights ||
      (highlights.ranges.length === 0 && !highlights.active)
    ) {
      setPageHighlights(highlightKey, [], []);
      return;
    }
    const toRanges = ([s, e]: readonly [number, number]): Range[] => {
      const ranges: Range[] = [];
      for (const [index, start, end] of matchPieces(text.page.segments, s, e)) {
        const node = text.divs[index]?.firstChild;
        if (!node) continue;
        const range = document.createRange();
        range.setStart(node, Math.min(start, node.textContent?.length ?? 0));
        range.setEnd(node, Math.min(end, node.textContent?.length ?? 0));
        ranges.push(range);
      }
      return ranges;
    };
    const all = highlights.ranges.flatMap(toRanges);
    const activeRanges = highlights.active ? toRanges(highlights.active) : [];
    const active = activeRanges[0] ?? null;
    const ocrIndex = highlights.active
      ? matchPieces(text.page.segments, ...highlights.active).find(([index]) =>
          text.ocr.has(index),
        )?.[0]
      : undefined;
    const activeBlock =
      ocrIndex != null
        ? wrapRef.current?.querySelector(`[data-ocr-segment="${ocrIndex}"]`)
        : null;
    setPageHighlights(highlightKey, all, activeRanges);
    if ((active || activeBlock) && scrolledTo.current !== activeKey) {
      scrolledTo.current = activeKey;
      (activeBlock ?? active?.startContainer.parentElement)?.scrollIntoView({
        block: "center",
        inline: "nearest",
      });
    }
    if (!active && !activeBlock) scrolledTo.current = "";
  }, [text, highlights, highlightKey, activeKey]);
  React.useEffect(
    () => () => setPageHighlights(highlightKey, [], []),
    [highlightKey],
  );

  const ocrHits = new Map<number, { rect: PdfRect; active: boolean }>();
  if (text && highlights)
    for (const range of highlights.ranges) {
      for (const [index] of matchPieces(text.page.segments, ...range)) {
        const rect = text.ocr.get(index);
        if (rect)
          ocrHits.set(index, {
            rect,
            active:
              ocrHits.get(index)?.active ||
              (!!highlights.active &&
                range[0] === highlights.active[0] &&
                range[1] === highlights.active[1]),
          });
      }
    }
  const mine = boxes.filter((b) => b.page === pageNumber);
  const height = ratio
    ? width *
      ((ratio.rotation - rotation) % 180 === 0 ? ratio.value : 1 / ratio.value)
    : (estimate ?? width * 1.414);
  return (
    <div
      ref={wrapRef}
      data-page={pageNumber}
      className={cn(
        "relative overflow-hidden rounded-sm bg-white shadow-sm ring-1 ring-black/[0.06]",
        className,
      )}
      style={{
        width,
        height,
        ["--scale-factor" as string]: size?.viewport.scale ?? 1,
      }}
    >
      <canvas
        ref={canvasRef}
        className="absolute inset-0 h-full w-full"
        aria-hidden
      />
      {textLayer ? <div ref={textRef} className="pdf-text-layer" /> : null}
      {size &&
        Array.from(ocrHits, ([index, hit]) => {
          const css = boxToViewport(hit.rect, false, size.viewport);
          return (
            <div
              key={index}
              data-ocr-search-box=""
              data-ocr-segment={index}
              title="OCR の検索結果（段落・表単位）"
              className={cn(
                "pointer-events-none absolute z-[2] rounded-sm border border-dashed",
                hit.active
                  ? "border-orange-500 bg-orange-400/25"
                  : "border-yellow-500 bg-yellow-400/20",
              )}
              style={{
                left: css.left,
                top: css.top,
                width: css.width,
                height: css.height,
              }}
            >
              <span className="absolute bottom-full left-0 whitespace-nowrap rounded-t bg-background/90 px-1 text-[10px] text-foreground">
                OCR・段落／表単位
              </span>
            </div>
          );
        })}
      {size && regionSelection ? (
        <PdfRegionLayer
          viewport={size.viewport}
          page={pageNumber}
          enabled={
            regionSelection.enabled &&
            size.w === width &&
            size.rotation === rotation
          }
          selections={regionSelection.selections}
          precision={regionSelection.precision}
          units={fineUnits?.viewport === size.viewport ? fineUnits.values : []}
          unitsReady={fineUnits?.viewport === size.viewport}
          onSelect={regionSelection.onSelect}
        />
      ) : null}
      {size
        ? mine.map((b, i) => {
            const css = boxToViewport(b.bbox, b.topLeft, size.viewport);
            return (
              <div
                key={i}
                data-cite-box=""
                className="pointer-events-none absolute z-[2] rounded-[3px] bg-[var(--season-autumn)]/15 ring-2 ring-[var(--season-autumn)]/70"
                style={{
                  left: css.left - 3,
                  top: css.top - 3,
                  width: css.width + 6,
                  height: css.height + 6,
                }}
              />
            );
          })
        : null}
    </div>
  );
}
