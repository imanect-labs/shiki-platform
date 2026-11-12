import type { PageViewport } from "pdfjs-dist";
import { boxToViewport } from "./pdf-geometry";
import { rectFromViewport, type PdfRect } from "./pdf-excerpts";

export type RegionHandle =
  | "move"
  | "n"
  | "ne"
  | "e"
  | "se"
  | "s"
  | "sw"
  | "w"
  | "nw";
export type ScreenRect = [number, number, number, number];
export function screenRect(bbox: PdfRect, viewport: PageViewport): ScreenRect {
  const r = boxToViewport(bbox, false, viewport);
  return [r.left, r.top, r.left + r.width, r.top + r.height];
}
/// 操作方向は画面基準。確定時だけPDF座標へ戻し、回転・CropBoxを保存値へ混ぜない。
export function editRegion(
  rect: ScreenRect,
  handle: RegionHandle,
  dx: number,
  dy: number,
  width: number,
  height: number,
): ScreenRect {
  const r: ScreenRect = [...rect];
  if (handle === "move") {
    const x = Math.max(-r[0], Math.min(width - r[2], dx));
    const y = Math.max(-r[1], Math.min(height - r[3], dy));
    return [r[0] + x, r[1] + y, r[2] + x, r[3] + y];
  }
  if (handle.includes("w")) r[0] = Math.max(0, Math.min(r[2] - 8, r[0] + dx));
  if (handle.includes("e"))
    r[2] = Math.min(width, Math.max(r[0] + 8, r[2] + dx));
  if (handle.includes("n")) r[1] = Math.max(0, Math.min(r[3] - 8, r[1] + dy));
  if (handle.includes("s"))
    r[3] = Math.min(height, Math.max(r[1] + 8, r[3] + dy));
  return r;
}
export function pdfRect(rect: ScreenRect, viewport: PageViewport) {
  return rectFromViewport(viewport, [rect[0], rect[1]], [rect[2], rect[3]]);
}
