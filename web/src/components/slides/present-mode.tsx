"use client";

/// プレゼンテーションモード（Task 11.1）。全画面オーバーレイでスライドを 1 枚ずつ表示する。
/// 操作: ←/→（PageUp/PageDown）で移動・Escape で終了・クリックで次へ。

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

  const slide = slides[index];
  if (!slide) return null;

  return (
    <div
      role="dialog"
      aria-label="プレゼンテーション"
      data-testid="present-mode"
      className="fixed inset-0 z-50 flex flex-col bg-black"
    >
      <button
        type="button"
        onClick={onClose}
        aria-label="プレゼンテーションを終了"
        className="absolute right-4 top-4 z-10 flex size-9 items-center justify-center rounded-full bg-white/10 text-white/80 transition-colors hover:bg-white/20 hover:text-white"
      >
        <X className="size-5" aria-hidden />
      </button>
      {/* クリックで次へ（終端では終了しない・誤タップで閉じない） */}
      <div
        className="flex min-h-0 flex-1 cursor-pointer items-center justify-center p-6"
        onClick={() => step(1)}
      >
        <div className="w-full max-w-[min(100%,calc((100vh-8rem)*16/9))]">
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
