"use client";

/// プレゼンテーションモード（Task 11.1）。全画面オーバーレイでスライドを 1 枚ずつ表示する。
/// 操作: ←/→（PageUp/PageDown）で移動・Escape で終了。クリック/タップは左 1/3 で戻る・
/// それ以外で次へ。タッチでは横スワイプでも前後へ送る。

import { X } from "lucide-react";
import * as React from "react";

import { SlideFrame } from "@/components/slides/slide-frame";
import type { SlideData } from "@/lib/slides-api";

export function PresentMode({
  slides,
  initialIndex,
  onClose,
}: {
  slides: SlideData[];
  initialIndex: number;
  onClose: () => void;
}) {
  const [index, setIndex] = React.useState(() =>
    Math.min(Math.max(initialIndex, 0), Math.max(slides.length - 1, 0)),
  );

  const step = React.useCallback(
    (delta: number) => {
      setIndex((prev) => Math.min(Math.max(prev + delta, 0), Math.max(slides.length - 1, 0)));
    },
    [slides.length],
  );

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          onClose();
          break;
        case "ArrowRight":
        case "ArrowDown":
        case "PageDown":
        case " ":
          e.preventDefault();
          step(1);
          break;
        case "ArrowLeft":
        case "ArrowUp":
        case "PageUp":
          e.preventDefault();
          step(-1);
          break;
        default:
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, step]);

  // 横スワイプ（タッチ）。縦方向が勝つ動き・短い動きはタップとして扱う。
  const swipe = React.useRef<{ x: number; y: number } | null>(null);
  const swiped = React.useRef(false);
  const onPointerDown = (e: React.PointerEvent) => {
    swiped.current = false;
    if (e.pointerType === "mouse") return;
    swipe.current = { x: e.clientX, y: e.clientY };
  };
  const onPointerUp = (e: React.PointerEvent) => {
    const start = swipe.current;
    swipe.current = null;
    if (!start) return;
    const dx = e.clientX - start.x;
    const dy = e.clientY - start.y;
    if (Math.abs(dx) > 48 && Math.abs(dx) > Math.abs(dy) * 1.5) {
      swiped.current = true;
      step(dx < 0 ? 1 : -1);
    }
  };
  const onClick = (e: React.MouseEvent<HTMLDivElement>) => {
    // スワイプ直後の click は無視（二重送り防止）。
    if (swiped.current) {
      swiped.current = false;
      return;
    }
    const rect = e.currentTarget.getBoundingClientRect();
    step(e.clientX - rect.left < rect.width / 3 ? -1 : 1);
  };

  const slide = slides[index];
  if (!slide) return null;

  return (
    <div
      role="dialog"
      aria-label="プレゼンテーション"
      data-testid="present-mode"
      className="fixed inset-0 z-50 flex h-dvh flex-col overscroll-none bg-black pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)] pr-[env(safe-area-inset-right)] pt-[env(safe-area-inset-top)]"
    >
      <button
        type="button"
        onClick={onClose}
        aria-label="プレゼンテーションを終了"
        className="absolute right-[max(1rem,env(safe-area-inset-right))] top-[max(1rem,env(safe-area-inset-top))] z-10 flex size-11 items-center justify-center rounded-full bg-white/10 text-white/80 transition-colors hover:bg-white/20 hover:text-white md:size-9"
      >
        <X className="size-5" aria-hidden />
      </button>
      {/* 左 1/3 のクリック/タップで戻る・それ以外で次へ（終端では終了しない・誤タップで閉じない）。 */}
      <div
        className="flex min-h-0 flex-1 cursor-pointer touch-pan-y select-none items-center justify-center p-6 max-md:p-2"
        onClick={onClick}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerUp}
        onPointerCancel={() => (swipe.current = null)}
      >
        <div className="w-full max-w-[min(100%,calc((100dvh-8rem)*16/9))]">
          <SlideFrame
            slide={slide}
            title={`スライド ${index + 1}`}
            className="rounded-none border-0 shadow-none"
          />
        </div>
      </div>
      <div className="pb-4 text-center text-sm tabular-nums text-white/60">
        {index + 1} / {slides.length}
      </div>
    </div>
  );
}
