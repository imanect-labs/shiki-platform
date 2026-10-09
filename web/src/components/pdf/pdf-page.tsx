"use client";

/// PDF の 1 ページを描き、引用箇所の枠を重ねる（#508）。
///
/// 枠は Docling の prov（PDF ポイント座標）。ページの高さで上下を反転して重ねる。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { cn } from "@/lib/utils";
import { boxToCss } from "@/lib/pdf";
import type { CiteBox } from "@/lib/citation-locate";

export function PdfPage({
  doc,
  pageNumber,
  width,
  boxes = [],
  className,
  eager = false,
}: {
  doc: PDFDocumentProxy;
  pageNumber: number;
  /// 表示幅（CSS px）。高さはページの縦横比から決まる。
  width: number;
  boxes?: readonly CiteBox[];
  className?: string;
  /// 画面外でもすぐ描く（既定は画面に近づいたら描く）。
  eager?: boolean;
}) {
  const wrapRef = React.useRef<HTMLDivElement>(null);
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const [size, setSize] = React.useState<{ w: number; h: number; scale: number; pageHeight: number } | null>(null);
  const [visible, setVisible] = React.useState(eager);

  // 先にページの寸法だけ取り、枠の場所を確保する（スクロール位置がずれないように）。
  React.useEffect(() => {
    let active = true;
    void doc.getPage(pageNumber).then((page) => {
      if (!active) return;
      const base = page.getViewport({ scale: 1 });
      const scale = width / base.width;
      setSize({ w: width, h: base.height * scale, scale, pageHeight: base.height });
    });
    return () => {
      active = false;
    };
  }, [doc, pageNumber, width]);

  React.useEffect(() => {
    if (eager || visible) return;
    const el = wrapRef.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setVisible(true);
      },
      { rootMargin: "800px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [eager, visible]);

  React.useEffect(() => {
    if (!visible || !size) return;
    let task: { cancel: () => void; promise: Promise<void> } | null = null;
    let active = true;
    void doc.getPage(pageNumber).then((page) => {
      const canvas = canvasRef.current;
      if (!active || !canvas) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const viewport = page.getViewport({ scale: size.scale * dpr });
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      task = page.render({ canvasContext: ctx, viewport });
      task.promise.catch(() => undefined);
    });
    return () => {
      active = false;
      task?.cancel();
    };
  }, [doc, pageNumber, size, visible]);

  const mine = boxes.filter((b) => b.page === pageNumber);
  return (
    <div
      ref={wrapRef}
      data-page={pageNumber}
      className={cn("relative overflow-hidden rounded-sm bg-white shadow-sm ring-1 ring-black/[0.06]", className)}
      style={size ? { width: size.w, height: size.h } : { width, height: width * 1.414 }}
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
