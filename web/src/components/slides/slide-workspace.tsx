"use client";

/// スライドワークスペース（Task 11.1/11.2）。フィルムストリップ＋メイン領域を一元ホストし、
/// editor 権限なら GrapesJS 砂箱エディタ、viewer なら安全な閲覧フレームを出す。
/// スライドの追加/削除/並べ替え・ノート編集は親（ここ）が Yjs へ書く。

import {
  ChevronDown,
  ChevronUp,
  FileDown,
  Plus,
  Presentation,
  Sparkles,
  Trash2,
} from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { ExportDialog } from "@/components/slides/export-dialog";
import { PresentMode } from "@/components/slides/present-mode";
import { SlideEditorHost } from "@/components/slides/slide-editor-host";
import { SlideFrame } from "@/components/slides/slide-frame";
import { useSlides } from "@/components/slides/use-slides";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { addSlide, moveSlide, removeSlide, updateSlideNotes } from "@/lib/slides-doc";
import { cn } from "@/lib/utils";

export function SlideWorkspace({
  doc,
  editable,
  name = "スライド",
  onAskAi,
}: {
  doc: Y.Doc;
  editable: boolean;
  /// ファイル名（拡張子なし・pptx エクスポート名に使う）。
  name?: string;
  /// 選択→AI 指示（Task 11.10。未指定ならボタンを出さない）。
  onAskAi?: (selection: { slideId: string; html: string }) => void;
}) {
  const slides = useSlides(doc);
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  const [presenting, setPresenting] = React.useState(false);
  const [exporting, setExporting] = React.useState(false);
  // キャンバスの要素選択（選択→AI 指示・Task 11.10）。
  const [canvasSelection, setCanvasSelection] = React.useState<{
    slideId: string;
    html: string;
  } | null>(null);
  // エディタバンドル未配備（/builtin 404）時の閲覧フォールバック。
  const [editorUnavailable, setEditorUnavailable] = React.useState(false);

  // 選択の解決: 未選択/消滅時は先頭スライドへ寄せる。
  const selected = slides.find((s) => s.id === selectedId) ?? slides[0] ?? null;
  const selectedIndex = selected ? slides.findIndex((s) => s.id === selected.id) : -1;

  const editing = editable && !editorUnavailable;

  if (slides.length === 0 && !editing) {
    return (
      <EmptyState
        title="スライドがありません"
        description="このドキュメントにはまだスライドがありません。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0">
      {/* フィルムストリップ（選択=塗り bg-accent） */}
      <div
        className="flex w-48 shrink-0 flex-col border-r border-border/60"
        data-testid="slide-filmstrip"
      >
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
          {slides.map((slide, i) => (
            <button
              key={slide.id}
              type="button"
              onClick={() => setSelectedId(slide.id)}
              aria-label={`スライド ${i + 1} を表示`}
              aria-current={slide.id === selected?.id ? "true" : undefined}
              className={cn(
                "block w-full rounded-lg p-1.5 text-left transition-colors",
                slide.id === selected?.id ? "bg-accent" : "hover:bg-accent/50",
              )}
            >
              <SlideFrame slide={slide} title={`スライド ${i + 1} サムネイル`} />
              <div className="mt-1 px-1 text-xs tabular-nums text-muted-foreground">{i + 1}</div>
            </button>
          ))}
        </div>
        {editing ? (
          <div className="flex items-center gap-1 border-t border-border/60 p-2">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="flex-1"
              onClick={() => setSelectedId(addSlide(doc, selected?.id ?? null))}
              data-testid="slide-add"
            >
              <Plus className="size-4" aria-hidden />
              追加
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8"
              disabled={!selected || selectedIndex <= 0}
              onClick={() => selected && moveSlide(doc, selected.id, -1)}
              aria-label="スライドを上へ移動"
            >
              <ChevronUp className="size-4" aria-hidden />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8"
              disabled={!selected || selectedIndex >= slides.length - 1}
              onClick={() => selected && moveSlide(doc, selected.id, 1)}
              aria-label="スライドを下へ移動"
            >
              <ChevronDown className="size-4" aria-hidden />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8 text-destructive hover:text-destructive"
              disabled={!selected}
              onClick={() => selected && removeSlide(doc, selected.id)}
              aria-label="スライドを削除"
              data-testid="slide-remove"
            >
              <Trash2 className="size-4" aria-hidden />
            </Button>
          </div>
        ) : null}
      </div>

      {/* メイン領域 */}
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center justify-end gap-2 px-4 pt-3">
          {onAskAi && canvasSelection ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => onAskAi(canvasSelection)}
              data-testid="slide-ask-ai"
              className="text-primary"
            >
              <Sparkles className="mr-1.5 size-4" aria-hidden />
              選択を AI に依頼
            </Button>
          ) : null}
          {editorUnavailable ? (
            <span className="mr-auto text-xs text-muted-foreground">
              エディタバンドルが未配備のため閲覧表示です（管理者に確認してください）
            </span>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setExporting(true)}
            disabled={slides.length === 0}
            data-testid="slide-export"
          >
            <FileDown className="mr-1.5 size-4" aria-hidden />
            エクスポート
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setPresenting(true)}
            disabled={slides.length === 0}
            data-testid="slide-present"
          >
            <Presentation className="mr-1.5 size-4" aria-hidden />
            プレゼン
          </Button>
        </div>

        {editing ? (
          <div className="flex min-h-0 flex-1 flex-col gap-3 p-4">
            <div className="min-h-0 flex-1">
              <SlideEditorHost
                doc={doc}
                slideId={selected?.id ?? null}
                onUnavailable={() => setEditorUnavailable(true)}
                onSelection={setCanvasSelection}
              />
            </div>
            {selected ? (
              <textarea
                key={selected.id}
                defaultValue={selected.notes}
                onBlur={(e) => updateSlideNotes(doc, selected.id, e.target.value)}
                placeholder="スピーカーノート（発表者だけに見えるメモ）"
                rows={2}
                className="w-full resize-none rounded-lg border border-border/60 bg-card/40 px-3 py-2 text-sm outline-none transition-colors focus:border-primary/40"
                data-testid="slide-notes-input"
              />
            ) : null}
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 overflow-y-auto px-6 py-4">
            <div className="w-full max-w-[min(72rem,calc((100vh-14rem)*16/9))]">
              {selected ? (
                <SlideFrame
                  slide={selected}
                  title={`スライド ${selectedIndex + 1}`}
                  className="shadow-md"
                />
              ) : null}
              {selected?.notes ? (
                <div className="mt-4 rounded-lg border border-border/60 bg-card/40 p-4">
                  <div className="mb-1 text-xs font-medium text-muted-foreground">
                    スピーカーノート
                  </div>
                  <p className="whitespace-pre-wrap text-sm text-foreground">{selected.notes}</p>
                </div>
              ) : null}
            </div>
          </div>
        )}
      </div>

      {presenting ? (
        <PresentMode
          slides={slides}
          initialIndex={Math.max(selectedIndex, 0)}
          onClose={() => setPresenting(false)}
        />
      ) : null}
      {exporting ? (
        <ExportDialog doc={doc} name={name} open={exporting} onClose={() => setExporting(false)} />
      ) : null}
    </div>
  );
}
