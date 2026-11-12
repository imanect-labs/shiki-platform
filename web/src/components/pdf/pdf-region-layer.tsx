"use client";

import * as React from "react";
import type { PageViewport } from "pdfjs-dist";
import { boxToViewport } from "@/lib/pdf";
import {
  rectFromViewport,
  type PdfRect,
  type PdfExcerpt,
} from "@/lib/pdf-excerpts";
import {
  editRegion,
  pdfRect,
  screenRect,
  type RegionHandle,
  type ScreenRect,
} from "@/lib/pdf-region-edit";

import {
  fineContent,
  type FineUnit,
  type FineContent,
  type SelectionPrecision,
} from "@/lib/pdf-fine-selection";

const handles: {
  key: RegionHandle;
  label: string;
  x: number;
  y: number;
  cursor: string;
}[] = [
  { key: "nw", label: "左上", x: 0, y: 0, cursor: "nwse-resize" },
  { key: "n", label: "上", x: 50, y: 0, cursor: "ns-resize" },
  { key: "ne", label: "右上", x: 100, y: 0, cursor: "nesw-resize" },
  { key: "e", label: "右", x: 100, y: 50, cursor: "ew-resize" },
  { key: "se", label: "右下", x: 100, y: 100, cursor: "nwse-resize" },
  { key: "s", label: "下", x: 50, y: 100, cursor: "ns-resize" },
  { key: "sw", label: "左下", x: 0, y: 100, cursor: "nesw-resize" },
  { key: "w", label: "左", x: 0, y: 50, cursor: "ew-resize" },
];
export function PdfRegionLayer({
  viewport,
  page,
  enabled,
  selections,
  onSelect,
  precision = "area",
  units = [],
  unitsReady = true,
  unitsError,
}: {
  viewport: PageViewport;
  page: number;
  enabled: boolean;
  selections: PdfExcerpt[];
  precision?: SelectionPrecision;
  units?: FineUnit[];
  unitsReady?: boolean;
  unitsError?: string;
  onSelect: (
    page: number,
    bbox: PdfRect,
    replaceId?: string,
    fine?: FineContent,
  ) => void;
}) {
  const ref = React.useRef<HTMLDivElement>(null);
  const drag = React.useRef<{
    pointer: number;
    start: [number, number];
    bounds: ScreenRect | null;
    handle: RegionHandle;
    id?: string;
    viewport: PageViewport;
  } | null>(null);
  const keyboard = React.useRef<{ bounds: ScreenRect; id?: string } | null>(
    null,
  );
  const [preview, setPreview] = React.useState<{
    bbox: PdfRect;
    id?: string;
  } | null>(null);
  const [note, setNote] = React.useState("");
  const [active, setActive] = React.useState<string | null>(null);
  const cancel = () => {
    drag.current = null;
    keyboard.current = null;
    setPreview(null);
  };
  React.useEffect(() => {
    drag.current = null;
    keyboard.current = null;
    setPreview(null);
  }, [viewport, enabled, precision]);
  React.useEffect(() => {
    setNote("");
  }, [viewport, precision]);
  const point = (e: React.PointerEvent): [number, number] => {
    const r = ref.current!.getBoundingClientRect();
    return [
      Math.max(
        0,
        Math.min(
          viewport.width,
          ((e.clientX - r.left) * viewport.width) / r.width,
        ),
      ),
      Math.max(
        0,
        Math.min(
          viewport.height,
          ((e.clientY - r.top) * viewport.height) / r.height,
        ),
      ),
    ];
  };
  const begin = (
    e: React.PointerEvent,
    id?: string,
    handle: RegionHandle = "move",
  ) => {
    if (!enabled || e.button !== 0 || !e.isPrimary) return;
    const chosen = selections.find((s) => s.id === id);
    const kind = chosen ? (chosen.precision ?? "area") : precision;
    if (kind !== "area" && (!unitsReady || unitsError)) return;
    e.preventDefault();
    e.stopPropagation();
    cancel();
    setActive(id ?? null);
    (e.currentTarget as HTMLElement).focus({ preventScroll: true });
    e.currentTarget.setPointerCapture(e.pointerId);
    const selected = selections.find((s) => s.id === id);
    drag.current = {
      pointer: e.pointerId,
      start: point(e),
      bounds: selected ? screenRect(selected.bbox, viewport) : null,
      handle,
      id,
      viewport,
    };
  };
  const commit = (bbox: PdfRect, id?: string) => {
    const selected = selections.find((s) => s.id === id);
    const kind = selected ? (selected.precision ?? "area") : precision;
    if (kind !== "area" && (!unitsReady || unitsError)) return;
    let fine: FineContent | null | undefined;
    try {
      fine = kind !== "area" ? fineContent(units, bbox, kind) : undefined;
    } catch (e) {
      setNote(
        e instanceof Error ? e.message : "選択範囲を小さくしてください。",
      );
      return;
    }
    if (fine === null) {
      setNote(
        "この位置に細かい文字座標がありません。自由範囲で選択してください。",
      );
      return;
    }
    setNote("");
    onSelect(page, bbox, id, fine);
  };
  const moved = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!enabled || !d || d.pointer !== e.pointerId || d.viewport !== viewport)
      return;
    const p = point(e);
    return {
      d,
      bbox: d.bounds
        ? pdfRect(
            editRegion(
              d.bounds,
              d.handle,
              p[0] - d.start[0],
              p[1] - d.start[1],
              viewport.width,
              viewport.height,
            ),
            viewport,
          )
        : rectFromViewport(viewport, d.start, p),
      distance: Math.hypot(p[0] - d.start[0], p[1] - d.start[1]),
    };
  };
  const pointerEvents = {
    onPointerMove: (e: React.PointerEvent) => {
      const value = moved(e);
      if (value) setPreview({ bbox: value.bbox, id: value.d.id });
    },
    onPointerUp: (e: React.PointerEvent) => {
      const value = moved(e);
      cancel();
      if (!value) return;
      const r = screenRect(value.bbox, viewport);
      if (
        value.distance > 2 &&
        (value.d.id || (r[2] - r[0] > 6 && r[3] - r[1] > 6))
      )
        commit(value.bbox, value.d.id);
      else if (!value.d.id && precision !== "area") {
        const p = pdfRect(
          [
            value.d.start[0],
            value.d.start[1],
            value.d.start[0],
            value.d.start[1],
          ],
          viewport,
        );
        const unit = units.find(
          (u) =>
            u.kind === precision &&
            p[0] >= u.bbox[0] &&
            p[0] <= u.bbox[2] &&
            p[1] >= u.bbox[1] &&
            p[1] <= u.bbox[3],
        );
        if (unit) commit(unit.bbox);
        else
          setNote(
            "この位置に細かい文字座標がありません。自由範囲で選択してください。",
          );
      }
    },
    onPointerCancel: () => cancel(),
    onLostPointerCapture: () => cancel(),
  };
  const keys = (
    e: React.KeyboardEvent,
    id?: string,
    handle: RegionHandle = "move",
  ) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cancel();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const chosen = selections.find((s) => s.id === id);
    const kind = chosen ? (chosen.precision ?? "area") : precision;
    if (kind !== "area" && (!unitsReady || unitsError)) return;
    if (e.key.startsWith("Arrow")) {
      e.preventDefault();
      e.stopPropagation();
      const selected = selections.find((s) => s.id === id);
      const bounds =
        keyboard.current && keyboard.current.id === id
          ? keyboard.current.bounds
          : selected
            ? screenRect(selected.bbox, viewport)
            : ([
                viewport.width * 0.2,
                viewport.height * 0.3,
                viewport.width * 0.7,
                viewport.height * 0.6,
              ] as ScreenRect);
      const step = Math.max(
        2,
        Math.min(viewport.width, viewport.height) * 0.02,
      );
      const dx =
        e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
      const dy = e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0;
      const next = editRegion(
        bounds,
        e.shiftKey ? "se" : handle,
        dx,
        dy,
        viewport.width,
        viewport.height,
      );
      keyboard.current = { bounds: next, id };
      setPreview({ bbox: pdfRect(next, viewport), id });
    }
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      const selected = selections.find((s) => s.id === id);
      const bounds =
        keyboard.current && keyboard.current.id === id
          ? keyboard.current.bounds
          : selected
            ? screenRect(selected.bbox, viewport)
            : ([0, 0, viewport.width, viewport.height] as ScreenRect);
      commit(pdfRect(bounds, viewport), id);
      cancel();
    }
  };
  const draw = (bbox: PdfRect, key: string, index?: number) => {
    const r = boxToViewport(bbox, false, viewport);
    const editing = enabled && index !== undefined;
    const focused = active === key;
    return (
      <div
        key={key}
        data-bbox={JSON.stringify(bbox)}
        data-selection-id={key}
        data-testid={
          key === "preview" ? "pdf-region-preview" : "pdf-region-box"
        }
        role={editing ? "group" : undefined}
        aria-label={editing ? `抜粋${index! + 1}の選択枠` : undefined}
        onFocus={() => setActive(key)}
        className={`absolute rounded-[3px] border-2 border-primary bg-primary/10 shadow-[0_0_0_1px_white] ${editing ? "pointer-events-none touch-none focus-within:ring-2 focus-within:ring-primary/40" : "pointer-events-none"}`}
        style={{
          left: `${(r.left / viewport.width) * 100}%`,
          top: `${(r.top / viewport.height) * 100}%`,
          width: `${(r.width / viewport.width) * 100}%`,
          height: `${(r.height / viewport.height) * 100}%`,
          zIndex: focused ? 2 : 1,
        }}
      >
        {editing && (
          <button
            type="button"
            data-region-move=""
            aria-label={`抜粋${index! + 1}を調整。矢印で移動、Shiftと矢印でサイズ、Enterで確定`}
            className="pointer-events-auto absolute inset-0 cursor-move touch-none focus:outline-none"
            onPointerDown={(e) => begin(e, key)}
            onKeyDown={(e) => keys(e, key)}
            {...pointerEvents}
          />
        )}
        {index !== undefined && (
          <span className="absolute -left-2 -top-3 flex size-5 items-center justify-center rounded-full bg-primary text-[10px] font-bold text-primary-foreground shadow-sm">
            {index + 1}
          </span>
        )}
        {editing &&
          focused &&
          handles.map((h) => (
            <button
              key={h.key}
              type="button"
              aria-label={`抜粋${index + 1}の${h.label}を調整`}
              data-region-handle={h.key}
              className="pointer-events-auto absolute -translate-x-1/2 -translate-y-1/2 rounded-sm border-2 border-primary bg-background p-0 size-3.5 before:absolute before:-inset-3.5 focus:outline-none focus:ring-2 focus:ring-primary"
              style={{ left: `${h.x}%`, top: `${h.y}%`, cursor: h.cursor }}
              onPointerDown={(e) => begin(e, key, h.key)}
              onKeyDown={(e) => keys(e, key, h.key)}
              {...pointerEvents}
            />
          ))}
      </div>
    );
  };
  return (
    <div ref={ref} className="pointer-events-none absolute inset-0 z-[4]">
      {enabled && (
        <div
          role="button"
          tabIndex={0}
          aria-label={`p.${page} の範囲を選択。ドラッグまたは矢印で位置、Shiftと矢印でサイズ、Enterで追加、Escapeで中止`}
          data-testid="pdf-region-layer"
          aria-busy={precision !== "area" && !unitsReady}
          className="pointer-events-auto absolute inset-0 cursor-crosshair touch-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary"
          onPointerDown={(e) => begin(e)}
          onKeyDown={(e) => keys(e)}
          {...pointerEvents}
        />
      )}
      {enabled &&
        precision !== "area" &&
        units
          .filter((u) => u.kind === precision)
          .map((unit, i) => {
            const r = boxToViewport(unit.bbox, false, viewport);
            return (
              <button
                key={i}
                type="button"
                aria-label={`${precision === "line" ? "行" : "セル"}を選択: ${unit.text.slice(0, 50)}`}
                data-testid="pdf-fine-unit"
                className="pointer-events-auto absolute border border-dashed border-primary/30 bg-primary/[0.02] hover:bg-primary/15 focus:outline-none focus:ring-2 focus:ring-primary"
                style={{
                  left: r.left,
                  top: r.top,
                  width: r.width,
                  height: r.height,
                }}
                onPointerDown={(e) => begin(e)}
                {...pointerEvents}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    e.stopPropagation();
                    commit(unit.bbox);
                  }
                }}
              />
            );
          })}
      {enabled && (precision !== "area" || unitsError) && (
        <span
          role={unitsError ? "alert" : "status"}
          className={
            !unitsReady || unitsError
              ? "pointer-events-none absolute top-2 left-2 z-10 max-w-[90%] rounded border bg-background/95 p-2 text-xs text-foreground shadow"
              : "pointer-events-none absolute bottom-2 left-2 rounded bg-background/95 px-2 py-1 text-[10px] text-muted-foreground"
          }
        >
          {unitsError ? (
            unitsError
          ) : !unitsReady ? (
            "文字の位置を確認中…"
          ) : (
            <>
              文字層のある{precision === "line" ? "行" : "表セル"}を選択 ·
              OCRは自由範囲で
            </>
          )}
        </span>
      )}
      {note && !unitsError && (
        <span
          role="status"
          className="absolute left-2 top-2 max-w-[90%] rounded bg-background p-2 text-xs text-foreground shadow"
        >
          {note}
        </span>
      )}
      <div className="pointer-events-none absolute inset-0 z-[1]">
        {selections.map((s, i) =>
          s.page === page
            ? draw(preview?.id === s.id ? preview.bbox : s.bbox, s.id, i)
            : null,
        )}
        {preview && !preview.id && draw(preview.bbox, "preview")}
      </div>
    </div>
  );
}
