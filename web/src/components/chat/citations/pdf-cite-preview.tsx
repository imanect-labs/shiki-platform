"use client";

/// 出典パネルの PDF プレビュー（引用箇所のページを描き、枠を重ねる・#508）。
///
/// 表示中のページの枠のまわりを見せる。`zoom="fit"` では、そのページの枠（複数なら全体）の幅が
/// 窓に収まる倍率に拡大し、枠の位置へスクロールする。全体はビューアで開く。
/// 読み込みに失敗したら `onUnavailable` で知らせる（呼び出し側は本文表示へ戻す）。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import type { Citation } from "@/lib/chat-api";
import type { CiteBox } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { cn } from "@/lib/utils";
import { PdfPage } from "@/components/pdf/pdf-page";

/// 枠の位置合わせを続ける時間（ページの寸法・倍率が後から決まるため、その間は追従する）。
const SETTLE_MS = 3000;
/// `"fit"` で文字が読める最小の大きさ（PDF の 1pt を何 px 以上で描くか）。10pt の本文が 10px になる。
/// 枠の幅に合わせるだけだと、スマホのシートでは 6px 前後、右カラムでも 8px 前後になり読みにくい。
/// 窓の幅によらず、枠が窓からはみ出しても（横スクロールで枠の左端に揃える）この大きさまでは拡大する。
const MIN_PX_PER_PT = 1;

export function PdfCitePreview({
  citation,
  zoom = 1,
  className,
  onUnavailable,
}: {
  citation: Citation;
  /// ページの拡大。`"fit"` は表示中のページの枠の幅が窓に収まる倍率にする（1〜2.2 倍・文字が読める大きさ以上）。
  zoom?: number | "fit";
  className?: string;
  onUnavailable?: () => void;
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
  // 表示するのは先頭の枠のページだけなので、倍率もそのページの枠だけで決める。
  const pageBoxes = React.useMemo(() => boxes.filter((b) => b.page === page), [boxes, page]);
  const [doc, setDoc] = React.useState<PDFDocumentProxy | null | "error">(null);
  const frameRef = React.useRef<HTMLDivElement>(null);

  // 窓の実際の幅（右カラムとシートで違う。狭い端末のシートでもはみ出さないように測る）。
  const [frameWidth, setFrameWidth] = React.useState(0);
  React.useEffect(() => {
    const el = frameRef.current;
    if (!el) return;
    const measure = () => setFrameWidth(el.clientWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [doc]);
  // 窓の内側の幅（左右の余白 8px ずつを除く）。
  const inner = Math.max(160, frameWidth - 16);

  // 枠の幅に合わせた倍率（ページの幅が分かってから決める）。
  const [fit, setFit] = React.useState(1);
  React.useEffect(() => {
    if (zoom !== "fit" || !doc || doc === "error" || page == null || pageBoxes.length === 0) return;
    let active = true;
    void doc
      .getPage(page)
      .then((p) => {
        const pageWidth = p.getViewport({ scale: 1 }).width;
        const left = Math.min(...pageBoxes.map((b) => Math.min(b.bbox[0], b.bbox[2])));
        const right = Math.max(...pageBoxes.map((b) => Math.max(b.bbox[0], b.bbox[2])));
        // 枠の左右に 16pt の余白を見て、その幅が窓に収まる倍率。ただし文字が読める大きさは下回らない。
        const boxFit = pageWidth / (right - left + 32);
        const readable = (pageWidth * MIN_PX_PER_PT) / inner;
        if (active) setFit(Math.min(2.2, Math.max(1, boxFit, readable)));
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [zoom, doc, page, pageBoxes, inner]);
  const scale = zoom === "fit" ? fit : zoom;

  React.useEffect(() => {
    if (typeof citation.version !== "number" || page == null) return;
    let active = true;
    loadPdf(citation.node_id, citation.version)
      // ページ番号が PDF の範囲外なら、そのページは描けない（読み込み失敗と同じに扱う）。
      .then((d) => {
        if (page < 1 || page > d.numPages) throw new Error("page out of range");
        return d.getPage(page).then(() => d);
      })
      .then((d) => active && setDoc(d))
      .catch(() => {
        if (!active) return;
        setDoc("error");
        onUnavailable?.();
      });
    return () => {
      active = false;
    };
    // onUnavailable は呼び出し側で毎回作られ得るので依存に入れない（読み込み直しを起こさない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [citation.node_id, citation.version, page]);

  // 枠（複数なら全体）が窓の中央に来るようスクロールする。ページの寸法と倍率は後から決まるので、
  // しばらくの間は枠の位置が変わるたびに合わせ直す（古い位置で一度だけ合わせて終わらない）。
  React.useEffect(() => {
    if (!doc || doc === "error") return;
    const frame = frameRef.current;
    if (!frame) return;
    let last = "";
    const started = Date.now();
    const id = window.setInterval(() => {
      if (Date.now() - started > SETTLE_MS) window.clearInterval(id);
      const els = Array.from(frame.querySelectorAll<HTMLElement>("[data-cite-box]"));
      if (els.length === 0) return;
      // 枠の位置はページ（PdfPage）からの相対値。ページの窓内の位置を足して窓の座標にする。
      const pageEl = els[0].offsetParent as HTMLElement | null;
      const top = Math.min(...els.map((b) => b.offsetTop));
      const bottom = Math.max(...els.map((b) => b.offsetTop + b.offsetHeight));
      const left = Math.min(...els.map((b) => b.offsetLeft));
      const right = Math.max(...els.map((b) => b.offsetLeft + b.offsetWidth));
      const key = [top, bottom, left, right, pageEl?.offsetWidth].join(",");
      if (key === last) return;
      last = key;
      frame.scrollTop = Math.max(0, (pageEl?.offsetTop ?? 0) + (top + bottom) / 2 - frame.clientHeight / 2);
      // 横は枠の左端に揃える（収まるなら中央）。
      const pageLeft = pageEl?.offsetLeft ?? 0;
      frame.scrollLeft =
        right - left + 24 <= frame.clientWidth
          ? Math.max(0, pageLeft + (left + right) / 2 - frame.clientWidth / 2)
          : Math.max(0, pageLeft + left - 12);
    }, 100);
    return () => window.clearInterval(id);
  }, [doc, scale, inner]);

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
      {doc && frameWidth > 0 ? (
        <PdfPage
          doc={doc}
          pageNumber={page}
          width={Math.round(inner * scale)}
          boxes={pageBoxes}
          eager
          className="mx-auto"
        />
      ) : (
        <div className="h-full animate-pulse rounded-md bg-muted" />
      )}
    </div>
  );
}
