"use client";

/// スライド 1 枚の安全な描画フレーム（Task 11.1・design §4.8.3・PIT-40 第2/3層）。
///
/// - **DOMPurify**（第2層）: 共同編集の WS 直伝搬はサーバの書込サニタイズを通らないため、
///   描画直前にクライアントでも必ずサニタイズする。
/// - **srcdoc ＋ `sandbox=""`**（第3層): scripts を含む全能力を拒否した opaque origin で
///   描画する。仮に上流 2 層をすり抜けても実行コンテキストが無い。
/// - 論理キャンバスは 1280×720（16:9）。コンテナ幅に合わせて transform で等倍縮尺する。

import createDOMPurify from "dompurify";
import * as React from "react";

import type { SlideData } from "@/lib/slides-api";
import { cn } from "@/lib/utils";

/// スライドの論理キャンバス寸法（pptx エクスポートの計測基準と揃える）。
export const SLIDE_WIDTH = 1280;
export const SLIDE_HEIGHT = 720;

const purify = typeof window !== "undefined" ? createDOMPurify(window) : null;

/// サーバ（ammonia）と同方針のクライアント側サニタイズ。
function sanitize(html: string): string {
  if (!purify) return "";
  return purify.sanitize(html, {
    FORBID_TAGS: ["script", "iframe", "object", "embed", "form", "input", "style", "link", "meta"],
    FORBID_ATTR: ["srcset", "formaction", "xlink:href"],
    USE_PROFILES: { html: true, svg: true },
  });
}

/// 背景指定（bg.color）を安全な CSS 色として解釈する（url() 等の混入は拒否）。
function safeBackground(bg: SlideData["bg"]): string | undefined {
  const color = bg?.color;
  if (typeof color !== "string") return undefined;
  return /^[#a-zA-Z0-9(),.%\s-]+$/.test(color) && !color.toLowerCase().includes("url")
    ? color
    : undefined;
}

/// srcdoc に埋める基本タイポグラフィ（プレーンな HTML でも整って見える既定値）。
const BASE_CSS = `
  *, *::before, *::after { box-sizing: border-box; }
  html, body { margin: 0; width: ${SLIDE_WIDTH}px; height: ${SLIDE_HEIGHT}px; overflow: hidden; }
  body {
    font-family: "Hiragino Sans", "Noto Sans JP", "Yu Gothic", system-ui, sans-serif;
    color: #1a1a1a; background: #ffffff;
    display: flex; flex-direction: column; justify-content: center;
    padding: 72px 96px; line-height: 1.5;
  }
  h1 { font-size: 64px; font-weight: 700; margin: 0 0 24px; letter-spacing: -0.01em; }
  h2 { font-size: 44px; font-weight: 700; margin: 0 0 20px; }
  h3 { font-size: 32px; font-weight: 600; margin: 0 0 16px; }
  p, li { font-size: 26px; margin: 0 0 12px; }
  ul, ol { margin: 0 0 12px; padding-left: 1.4em; }
  table { border-collapse: collapse; font-size: 22px; }
  td, th { border: 1px solid #d4d4d4; padding: 8px 14px; text-align: left; }
  th { background: #f5f5f4; }
  img { max-width: 100%; }
  blockquote { border-left: 4px solid #d4d4d4; margin: 0 0 12px; padding: 4px 0 4px 20px; color: #555; }
  pre, code { font-family: ui-monospace, "SFMono-Regular", monospace; font-size: 22px; }
  pre { background: #f5f5f4; border-radius: 8px; padding: 16px 20px; overflow: hidden; }
`;

/// スライド 1 枚を 16:9 で描画する（親要素の幅に自動フィット）。
export function SlideFrame({
  slide,
  className,
  title,
}: {
  slide: SlideData;
  className?: string;
  title: string;
}) {
  const containerRef = React.useRef<HTMLDivElement | null>(null);
  const [scale, setScale] = React.useState(0);

  React.useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width ?? 0;
      setScale(width / SLIDE_WIDTH);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const srcDoc = React.useMemo(() => {
    const body = sanitize(slide.html);
    const background = safeBackground(slide.bg);
    const bgCss = background ? `body { background: ${background}; }` : "";
    return `<!doctype html><html><head><meta charset="utf-8"><style>${BASE_CSS}${bgCss}</style></head><body>${body}</body></html>`;
  }, [slide.html, slide.bg]);

  return (
    <div
      ref={containerRef}
      className={cn(
        "relative aspect-video w-full overflow-hidden rounded-lg border border-border/60 bg-white shadow-sm",
        className,
      )}
      data-testid="slide-frame"
    >
      {scale > 0 ? (
        <iframe
          title={title}
          // 全能力拒否の sandbox（allow-scripts すら付けない・PIT-40 第3層）。
          sandbox=""
          srcDoc={srcDoc}
          className="pointer-events-none absolute left-0 top-0 origin-top-left border-0"
          style={{ width: SLIDE_WIDTH, height: SLIDE_HEIGHT, transform: `scale(${scale})` }}
        />
      ) : null}
    </div>
  );
}
