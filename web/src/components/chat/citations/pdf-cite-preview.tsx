"use client";

/// 出典パネルの PDF プレビュー（引用箇所のページを縮小で描き、枠を重ねる・#508）。
///
/// 枠のまわりの帯だけを見せる（ページ全体を出すと余白ばかりで、下の原文を画面外へ押し出す）。
/// 枠が窓の縦の中央に来るようスクロールしておく。全体はビューアで開く。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import type { Citation } from "@/lib/chat-api";
import type { CiteBox } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { cn } from "@/lib/utils";
import { PdfPage } from "@/components/pdf/pdf-page";

export function PdfCitePreview({
  citation,
  width,
  zoom = 1,
  className,
}: {
  citation: Citation;
  width: number;
  /// ページの拡大。`"fit"` は枠（複数なら全体）の幅が窓に収まる倍率にする（1〜2.2 倍）。
  zoom?: number | "fit";
  className?: string;
}) {
  const boxes: CiteBox[] = React.useMemo(
    () =>
      (citation.boxes ?? []).map((b) => ({
        page: b.page,
        bbox: [b.bbox[0], b.bbox[1], b.bbox[2], b.bbox[3]],
        topLeft: b.origin === "top_left",
      })),
    [citation.boxes],
  );
  const page = boxes[0]?.page ?? citation.page ?? null;
  const [doc, setDoc] = React.useState<PDFDocumentProxy | null | "error">(null);
  // 枠の幅に合わせた倍率（ページの幅が分かってから決める）。
  const [fit, setFit] = React.useState(1);
  React.useEffect(() => {
    if (zoom !== "fit" || !doc || doc === "error" || page == null || boxes.length === 0) return;
    let active = true;
    void doc
      .getPage(page)
      .then((p) => {
        const pageWidth = p.getViewport({ scale: 1 }).width;
        const left = Math.min(...boxes.map((b) => Math.min(b.bbox[0], b.bbox[2])));
        const right = Math.max(...boxes.map((b) => Math.max(b.bbox[0], b.bbox[2])));
        // 枠の左右に 16pt の余白を見て、その幅が窓に収まる倍率。
        if (active) setFit(Math.min(2.2, Math.max(1, pageWidth / (right - left + 32))));
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [zoom, doc, page, boxes]);
  const scale = zoom === "fit" ? fit : zoom;
  const frameRef = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    if (typeof citation.version !== "number" || page == null) return;
    let active = true;
    loadPdf(citation.node_id, citation.version)
      .then((d) => active && setDoc(d))
      .catch(() => active && setDoc("error"));
    return () => {
      active = false;
    };
  }, [citation.node_id, citation.version, page]);

  // 枠が描かれたら、枠（複数なら全体）が窓の縦の中央に来るようスクロールする。
  React.useEffect(() => {
    if (!doc || doc === "error") return;
    const frame = frameRef.current;
    if (!frame) return;
    let tries = 0;
    const id = window.setInterval(() => {
      const boxes = Array.from(frame.querySelectorAll<HTMLElement>("[data-cite-box]"));
      if (boxes.length > 0 || ++tries > 20) window.clearInterval(id);
      if (boxes.length === 0) return;
      // 枠の位置はページ（PdfPage）からの相対値。ページの窓内の位置を足して窓の座標にする。
      const pageEl = boxes[0].offsetParent as HTMLElement | null;
      const top = Math.min(...boxes.map((b) => b.offsetTop));
      const bottom = Math.max(...boxes.map((b) => b.offsetTop + b.offsetHeight));
      const left = Math.min(...boxes.map((b) => b.offsetLeft));
      const right = Math.max(...boxes.map((b) => b.offsetLeft + b.offsetWidth));
      frame.scrollTop = Math.max(0, (pageEl?.offsetTop ?? 0) + (top + bottom) / 2 - frame.clientHeight / 2);
      // 横は枠の左端に揃える（収まるなら中央）。
      const span = right - left;
      const pageLeft = pageEl?.offsetLeft ?? 0;
      frame.scrollLeft =
        span + 24 <= frame.clientWidth
          ? Math.max(0, pageLeft + (left + right) / 2 - frame.clientWidth / 2)
          : Math.max(0, pageLeft + left - 12);
    }, 100);
    return () => window.clearInterval(id);
  }, [doc, scale]);

  if (page == null || typeof citation.version !== "number" || doc === "error") return null;
  return (
    <div
      ref={frameRef}
      className={cn(
        "relative h-36 rounded-lg bg-muted/50 p-2",
        scale > 1 ? "overflow-auto scrollbar-subtle" : "overflow-hidden",
        className,
      )}
      data-testid="pdf-cite-preview"
      aria-label={`p.${page} の引用箇所`}
    >
      {doc ? (
        <PdfPage doc={doc} pageNumber={page} width={Math.round((width - 16) * scale)} boxes={boxes} eager className="mx-auto" />
      ) : (
        <div className="h-full animate-pulse rounded-md bg-muted" />
      )}
    </div>
  );
}
