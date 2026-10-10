"use client";

/// PDF の 1 ページを描き、引用箇所の枠を重ねる（#508）。
///
/// 枠は Docling の prov（PDF ポイント座標）。ページの高さで上下を反転して重ねる。
///
/// 大きな PDF でも重くしないよう、**画面に近いページだけ**を描く。画面から離れたページは
/// canvas を空にしてメモリを返す（高 DPI では 1 ページ十数 MB になる）。寸法も近づいてから
/// 取り、それまでは `estimate`（1 ページ目の縦横比から出した高さ）で場所だけ確保する。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { cn } from "@/lib/utils";
import { boxToCss } from "@/lib/pdf";
import type { CiteBox } from "@/lib/citation-locate";

type Size = { w: number; h: number; scale: number; pageHeight: number };

export function PdfPage({
  doc,
  pageNumber,
  width,
  estimate,
  boxes = [],
  className,
  eager = false,
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
}) {
  const wrapRef = React.useRef<HTMLDivElement>(null);
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const [size, setSize] = React.useState<Size | null>(null);
  const [near, setNear] = React.useState(eager);

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
        const base = page.getViewport({ scale: 1 });
        const scale = width / base.width;
        setSize({ w: width, h: base.height * scale, scale, pageHeight: base.height });
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [doc, pageNumber, width, near]);

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
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const viewport = page.getViewport({ scale: size.scale * dpr });
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

  const mine = boxes.filter((b) => b.page === pageNumber);
  const height = size?.h ?? estimate ?? width * 1.414;
  return (
    <div
      ref={wrapRef}
      data-page={pageNumber}
      className={cn("relative overflow-hidden rounded-sm bg-white shadow-sm ring-1 ring-black/[0.06]", className)}
      style={{ width, height }}
    >
      <canvas ref={canvasRef} className="absolute inset-0 h-full w-full" aria-hidden />
      {size
        ? mine.map((b, i) => {
            const css = boxToCss(b.bbox, b.topLeft, size.pageHeight, size.scale);
            return (
              <div
                key={i}
                data-cite-box=""
                className="pointer-events-none absolute rounded-[3px] bg-[var(--season-autumn)]/15 ring-2 ring-[var(--season-autumn)]/70"
                style={{ left: css.left - 3, top: css.top - 3, width: css.width + 6, height: css.height + 6 }}
              />
            );
          })
        : null}
    </div>
  );
}
