"use client";

import * as React from "react";
import type { PageViewport } from "pdfjs-dist";
import { boxToViewport } from "@/lib/pdf";
import {
  rectFromViewport,
  type PdfRect,
  type PdfExcerpt,
} from "@/lib/pdf-excerpts";

export function PdfRegionLayer({
  viewport,
  page,
  enabled,
  selections,
  onSelect,
}: {
  viewport: PageViewport;
  page: number;
  enabled: boolean;
  selections: PdfExcerpt[];
  onSelect: (page: number, bbox: PdfRect) => void;
}) {
  const ref = React.useRef<HTMLDivElement>(null);
  const drag = React.useRef<{
    pointer: number;
    start: [number, number];
    rect: PdfRect | null;
  } | null>(null);
  const [preview, setPreview] = React.useState<PdfRect | null>(null);
  // 表示変換が替わった瞬間、途中のドラッグは無効化する。確定済み範囲は PDF 座標のまま残す。
  React.useEffect(() => {
    drag.current = null;
    setPreview(null);
  }, [viewport, enabled]);
  const point = (e: React.PointerEvent): [number, number] => {
    const r = ref.current!.getBoundingClientRect();
    return [
      Math.min(
        viewport.width,
        Math.max(0, ((e.clientX - r.left) * viewport.width) / r.width),
      ),
      Math.min(
        viewport.height,
        Math.max(0, ((e.clientY - r.top) * viewport.height) / r.height),
      ),
    ];
  };
  const finish = (e: React.PointerEvent, cancelled: boolean) => {
    const d = drag.current;
    if (!d || d.pointer !== e.pointerId) return;
    const rect = rectFromViewport(viewport, d.start, point(e));
    drag.current = null;
    setPreview(null);
    if (
      !cancelled &&
      (rect[2] - rect[0]) * viewport.scale > 6 &&
      (rect[3] - rect[1]) * viewport.scale > 6
    )
      onSelect(page, rect);
  };
  const draw = (bbox: PdfRect, key: string, index?: number) => {
    const r = boxToViewport(bbox, false, viewport);
    return (
      <div
        key={key}
        data-testid="pdf-region-box"
        className="pointer-events-none absolute rounded-[3px] border-2 border-primary bg-primary/10 shadow-[0_0_0_1px_white]"
        style={{
          left: `${(r.left / viewport.width) * 100}%`,
          top: `${(r.top / viewport.height) * 100}%`,
          width: `${(r.width / viewport.width) * 100}%`,
          height: `${(r.height / viewport.height) * 100}%`,
        }}
      >
        {index != null && (
          <span className="absolute -left-2 -top-3 flex size-5 items-center justify-center rounded-full bg-primary text-[10px] font-bold text-primary-foreground shadow-sm">
            {index + 1}
          </span>
        )}
      </div>
    );
  };
  return (
    <>
      <div className="pointer-events-none absolute inset-0 z-[3]">
        {selections.map((s, i) =>
          s.page === page ? draw(s.bbox, s.id, i) : null,
        )}
        {preview && draw(preview, "preview")}
      </div>
      {enabled && (
        <div
          ref={ref}
          role="button"
          tabIndex={0}
          aria-label={`p.${page} の範囲を選択。ドラッグで指定、Enterでページ全体、Escapeで中止`}
          data-testid="pdf-region-layer"
          className="absolute inset-0 z-[4] cursor-crosshair touch-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary"
          onPointerDown={(e) => {
            if (e.button !== 0 || !e.isPrimary) return;
            e.preventDefault();
            e.currentTarget.focus({ preventScroll: true });
            e.currentTarget.setPointerCapture(e.pointerId);
            drag.current = {
              pointer: e.pointerId,
              start: point(e),
              rect: null,
            };
          }}
          onPointerMove={(e) => {
            const d = drag.current;
            if (!d || d.pointer !== e.pointerId) return;
            d.rect = rectFromViewport(viewport, d.start, point(e));
            setPreview(d.rect);
          }}
          onPointerUp={(e) => finish(e, false)}
          onPointerCancel={(e) => finish(e, true)}
          onLostPointerCapture={() => {
            drag.current = null;
            setPreview(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              drag.current = null;
              setPreview(null);
            }
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onSelect(
                page,
                rectFromViewport(
                  viewport,
                  [0, 0],
                  [viewport.width, viewport.height],
                ),
              );
            }
          }}
        />
      )}
    </>
  );
}
