"use client";

/// 出典パネルの PDF プレビュー（引用箇所のページを縮小で描き、枠を重ねる・#508）。
///
/// 枠のあたりが見えるよう、ページを小さな窓の中でスクロールしておく。全体はビューアで開く。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import type { Citation } from "@/lib/chat-api";
import type { CiteBox } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { PdfPage } from "@/components/pdf/pdf-page";

export function PdfCitePreview({ citation, width }: { citation: Citation; width: number }) {
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

  // 枠が描かれたら、窓の中で枠が上から 1/3 あたりに来るようスクロールする。
  React.useEffect(() => {
    if (!doc || doc === "error") return;
    const frame = frameRef.current;
    if (!frame) return;
    let tries = 0;
    const id = window.setInterval(() => {
      const box = frame.querySelector<HTMLElement>("[data-cite-box]");
      if (box || ++tries > 20) window.clearInterval(id);
      if (box) frame.scrollTop = Math.max(0, box.offsetTop - frame.clientHeight / 3);
    }, 100);
    return () => window.clearInterval(id);
  }, [doc]);

  if (page == null || typeof citation.version !== "number" || doc === "error") return null;
  return (
    <div
      ref={frameRef}
      className="mb-2.5 max-h-60 overflow-hidden rounded-lg bg-muted/50 p-2"
      data-testid="pdf-cite-preview"
      aria-label={`p.${page} の引用箇所`}
    >
      {doc ? (
        <PdfPage doc={doc} pageNumber={page} width={width - 16} boxes={boxes} eager className="mx-auto" />
      ) : (
        <div className="h-40 animate-pulse rounded-md bg-muted" />
      )}
    </div>
  );
}
