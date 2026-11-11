import type { PageViewport } from "pdfjs-dist";

/// PDF 座標（ポイント・原点は左下か左上）の枠を、表示上の CSS ボックスへ写す。
/// viewport が回転・倍率を持つので、回転表示でも枠が文字に重なる。
export function boxToViewport(
  bbox: readonly [number, number, number, number],
  topLeft: boolean,
  viewport: PageViewport,
): { left: number; top: number; width: number; height: number } {
  const [l, t, r, b] = bbox;
  const { pageHeight, pageX, pageY } = viewport.rawDims as {
    pageHeight: number;
    pageX: number;
    pageY: number;
  };
  // 左上原点の座標は PDF の左下原点へ直す（ページの高さで上下反転）。
  const y1 = topLeft ? pageHeight - t : t;
  const y2 = topLeft ? pageHeight - b : b;
  const [x1, vy1, x2, vy2] = viewport.convertToViewportRectangle([
    l + pageX,
    y1 + pageY,
    r + pageX,
    y2 + pageY,
  ]);
  return {
    left: Math.min(x1, x2),
    top: Math.min(vy1, vy2),
    width: Math.abs(x2 - x1),
    height: Math.abs(vy2 - vy1),
  };
}

export function viewportDimensions(viewport: PageViewport) {
  return viewport.rawDims as {
    pageX: number;
    pageY: number;
    pageWidth: number;
    pageHeight: number;
  };
}
