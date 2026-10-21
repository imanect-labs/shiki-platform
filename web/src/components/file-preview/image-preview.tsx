"use client";

/// 画像のプレビュー（#784）。
///
/// 既定は画面に収める（小さい画像は等倍のまま）。拡大縮小はボタン・⌘＋／⌘−・Ctrl+ホイール・
/// ピンチで、ダブルクリック（ダブルタップ）で「画面に合わせる」と「等倍」を切り替える。
/// 倍率を変えても、指やカーソルの下にあった場所がそのまま残るようにスクロールを合わせる。
/// 透過のある画像が背景に溶けないよう、市松模様の上に置く。SVG も `<img>` で出す（スクリプトは動かない）。
import * as React from "react";
import { Maximize, Minus, Plus } from "lucide-react";

import { usePinchZoom } from "@/components/pdf/use-pinch-zoom";
import { EditorLoading } from "@/components/shell/editor-loading";
import { ChromeIconButton } from "@/components/shell/page-header-context";
import { nodeKindLabel } from "@/components/drive/primitives";
import { downloadUrl, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { useFileChrome } from "./file-chrome";
import type { PreviewFailure } from "./text-preview";

const ZOOM_MIN = 0.05;
const ZOOM_MAX = 8;
const ZOOM_STEPS = [0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8];
/// 画像の周りの余白（CSS px）。
const PAD = 16;

type Focal = { x: number; y: number };

export function ImagePreview({
  node,
  onDownload,
  onUnsupported,
}: {
  node: NodeResponse;
  onDownload: () => void;
  onUnsupported: (why: PreviewFailure) => void;
}) {
  const [src, setSrc] = React.useState<string | null>(null);
  const [natural, setNatural] = React.useState<{ w: number; h: number } | null>(null);
  const [box, setBox] = React.useState({ w: 0, h: 0 });
  // null は「画面に合わせる」。数値は等倍に対する倍率。
  const [zoom, setZoom] = React.useState<number | null>(null);
  const rootRef = React.useRef<HTMLDivElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;

  React.useEffect(() => {
    let active = true;
    downloadUrl(node.id)
      .then(({ url }) => active && setSrc(url))
      .catch(() => active && failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" }));
    return () => {
      active = false;
    };
  }, [node.id]);

  React.useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setBox({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 画面に収める倍率（小さい画像は引き伸ばさない）。
  const fit = natural && box.w > 0
    ? Math.min(1, (box.w - PAD * 2) / natural.w, (box.h - PAD * 2) / natural.h)
    : 1;
  const scale = zoom ?? fit;

  // 倍率を変えたら、焦点の下にあった画像上の点を同じ場所に戻す。
  const anchorRef = React.useRef<{ px: number; py: number; focal: Focal } | null>(null);
  const zoomTo = React.useCallback(
    (next: number | null, focal?: Focal) => {
      const root = rootRef.current;
      const content = contentRef.current;
      if (root && content) {
        const f = focal ?? { x: root.clientWidth / 2, y: root.clientHeight / 2 };
        const img = content.querySelector("img");
        const left = img ? img.offsetLeft : 0;
        const top = img ? img.offsetTop : 0;
        anchorRef.current = {
          px: (root.scrollLeft + f.x - left) / scale,
          py: (root.scrollTop + f.y - top) / scale,
          focal: f,
        };
      }
      setZoom(next == null ? null : Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, next)));
    },
    [scale],
  );
  React.useLayoutEffect(() => {
    const a = anchorRef.current;
    const root = rootRef.current;
    const img = contentRef.current?.querySelector("img");
    if (!a || !root || !img) return;
    anchorRef.current = null;
    root.scrollLeft = a.px * scale + img.offsetLeft - a.focal.x;
    root.scrollTop = a.py * scale + img.offsetTop - a.focal.y;
  }, [scale]);

  const step = React.useCallback(
    (dir: 1 | -1) => {
      const next = dir > 0
        ? ZOOM_STEPS.find((s) => s > scale * 1.01) ?? ZOOM_MAX
        : [...ZOOM_STEPS].reverse().find((s) => s < scale * 0.99) ?? ZOOM_MIN;
      zoomTo(next);
    },
    [scale, zoomTo],
  );
  const toggleFit = React.useCallback(
    (focal?: Focal) => {
      // 画面に収まっている（等倍で全体が見える）画像では、等倍と同じなので何もしない。
      if (zoom == null && fit >= 1) return;
      zoomTo(zoom == null ? 1 : null, focal);
    },
    [zoom, fit, zoomTo],
  );

  usePinchZoom({
    enabled: natural != null,
    rootRef,
    contentRef,
    clamp: (ratio) => Math.min(ZOOM_MAX / scale, Math.max(ZOOM_MIN / scale, ratio)),
    onCommit: (ratio, focal) => zoomTo(scale * ratio, focal),
  });

  // キーボード（⌘＋／⌘−／⌘0）。
  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        step(1);
      } else if (e.key === "-") {
        e.preventDefault();
        step(-1);
      } else if (e.key === "0") {
        e.preventDefault();
        zoomTo(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [step, zoomTo]);

  const percent = Math.round(scale * 100);
  useFileChrome(
    node,
    onDownload,
    {
      subtitle: natural ? (
        <span className="inline-flex items-center gap-2">
          <span>
            {nodeKindLabel(node)}・{natural.w.toLocaleString("ja-JP")} × {natural.h.toLocaleString("ja-JP")}
          </span>
          <span className="tabular-nums" data-testid="image-zoom-percent">
            {percent}%
          </span>
        </span>
      ) : undefined,
      actions: natural ? (
        <>
          <ChromeIconButton label="縮小（⌘−）" disabled={scale <= ZOOM_MIN} onClick={() => step(-1)} className="max-md:hidden" data-testid="image-zoom-out">
            <Minus aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton label="拡大（⌘＋）" disabled={scale >= ZOOM_MAX} onClick={() => step(1)} className="max-md:hidden" data-testid="image-zoom-in">
            <Plus aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton label="画面に合わせる（⌘0）" pressed={zoom == null} onClick={() => zoomTo(null)} data-testid="image-fit">
            <Maximize aria-hidden />
          </ChromeIconButton>
        </>
      ) : undefined,
    },
    [natural, percent, scale, zoom, step, zoomTo],
  );

  const w = natural ? Math.max(1, Math.round(natural.w * scale)) : undefined;
  const h = natural ? Math.max(1, Math.round(natural.h * scale)) : undefined;
  return (
    <div className="relative h-full min-h-0">
      {!natural ? (
        <div className="absolute inset-0">
          <EditorLoading kind="doc" message="画像を読み込んでいます…" />
        </div>
      ) : null}
      <div
        ref={rootRef}
        className={cn("h-full overflow-auto bg-muted/40 touch-pan-x touch-pan-y outline-none", !natural && "invisible")}
        data-testid="image-preview"
        tabIndex={0}
      >
        <div
          ref={contentRef}
          className="flex min-h-full min-w-full w-max items-center justify-center"
          style={{ padding: PAD }}
        >
          {src ? (
            // 署名付き URL を直接読む（next/image の最適化サーバを経由させない）。
            // eslint-disable-next-line @next/next/no-img-element
            <img
              src={src}
              alt={node.name}
              draggable={false}
              onLoad={(e) => setNatural({ w: e.currentTarget.naturalWidth || 1, h: e.currentTarget.naturalHeight || 1 })}
              onError={() => failRef.current({ description: "この画像の形式はブラウザで表示できません。ダウンロードして、お使いのアプリで開いてください。" })}
              onDoubleClick={(e) => {
                const r = rootRef.current?.getBoundingClientRect();
                toggleFit(r ? { x: e.clientX - r.left, y: e.clientY - r.top } : undefined);
              }}
              className={cn(
                "max-w-none select-none rounded-sm shadow-sm ring-1 ring-black/[0.06]",
                "bg-[repeating-conic-gradient(var(--muted)_0_25%,transparent_0_50%)] bg-[length:16px_16px]",
                zoom == null && fit >= 1 ? "cursor-default" : zoom == null ? "cursor-zoom-in" : "cursor-zoom-out",
              )}
              style={{ width: w, height: h }}
              data-testid="image-preview-img"
            />
          ) : null}
        </div>
      </div>
    </div>
  );
}
