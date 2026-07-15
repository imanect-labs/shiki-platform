"use client";

/// スライド閲覧ビュー（Task 11.1）。左=フィルムストリップ・右=選択スライドの拡大表示。
/// 編集（GrapesJS 砂箱エディタ）は Task 11.2 で本ビューと切り替えになる。

import { Presentation } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { PresentMode } from "@/components/slides/present-mode";
import { SlideFrame } from "@/components/slides/slide-frame";
import { useSlides } from "@/components/slides/use-slides";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { cn } from "@/lib/utils";

export function SlideViewer({ doc }: { doc: Y.Doc }) {
  const slides = useSlides(doc);
  const [selected, setSelected] = React.useState(0);
  const [presenting, setPresenting] = React.useState(false);
  const current = slides[Math.min(selected, Math.max(slides.length - 1, 0))];

  if (slides.length === 0) {
    return (
      <EmptyState
        title="スライドがありません"
        description="このドキュメントにはまだスライドがありません。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0">
      {/* フィルムストリップ（選択=塗り bg-accent・黒枠は使わない） */}
      <div
        className="w-48 shrink-0 space-y-3 overflow-y-auto border-r border-border/60 p-3"
        data-testid="slide-filmstrip"
      >
        {slides.map((slide, i) => (
          <button
            key={slide.id}
            type="button"
            onClick={() => setSelected(i)}
            aria-label={`スライド ${i + 1} を表示`}
            aria-current={i === selected ? "true" : undefined}
            className={cn(
              "block w-full rounded-lg p-1.5 text-left transition-colors",
              i === selected ? "bg-accent" : "hover:bg-accent/50",
            )}
          >
            <SlideFrame slide={slide} title={`スライド ${i + 1} サムネイル`} />
            <div className="mt-1 px-1 text-xs tabular-nums text-muted-foreground">{i + 1}</div>
          </button>
        ))}
      </div>

      {/* メイン表示 */}
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center justify-end gap-2 px-4 pt-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setPresenting(true)}
            data-testid="slide-present"
          >
            <Presentation className="mr-1.5 size-4" aria-hidden />
            プレゼン
          </Button>
        </div>
        {/* 高さ・幅の双方に収まる最大サイズで垂直センタリング（上寄せの余白を作らない） */}
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 overflow-y-auto px-6 py-4">
          <div className="w-full max-w-[min(72rem,calc((100vh-14rem)*16/9))]">
            {current ? (
              <SlideFrame slide={current} title={`スライド ${selected + 1}`} className="shadow-md" />
            ) : null}
            {current?.notes ? (
              <div className="mt-4 rounded-lg border border-border/60 bg-card/40 p-4">
                <div className="mb-1 text-xs font-medium text-muted-foreground">
                  スピーカーノート
                </div>
                <p className="whitespace-pre-wrap text-sm text-foreground">{current.notes}</p>
              </div>
            ) : null}
          </div>
        </div>
      </div>

      {presenting ? (
        <PresentMode
          slides={slides}
          initialIndex={selected}
          onClose={() => setPresenting(false)}
        />
      ) : null}
    </div>
  );
}
