"use client";

/// スライドワークスペース（Task 11.1/11.2）。フィルムストリップ＋メイン領域を一元ホストし、
/// editor 権限なら GrapesJS 砂箱エディタ、viewer なら安全な閲覧フレームを出す。
/// スライドの追加/削除/並べ替え・ノート編集は親（ここ）が Yjs へ書く。
///
/// - PC（md 以上）: 左に縦のフィルムストリップ、右にメイン。
/// - スマホ（md 未満）: メインを全幅にし、ストリップはスライドの下に横スクロール（‹ › で前後）。
///   タッチかつ狭い画面では GrapesJS 編集を既定にせず閲覧表示にする（「それでも編集する」で切替）。
/// - エクスポート・プレゼンの開始はページのクローム（⋯ / PC のバー）が持ち、状態だけ受け取る。

import {
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Eye,
  Pencil,
  Plus,
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
import { useMediaQuery } from "@/hooks/use-media-query";
import { addSlide, moveSlide, removeSlide, updateSlideNotes } from "@/lib/slides-doc";
import { cn } from "@/lib/utils";

/// プレゼン・エクスポートの開閉（クロームから開くため親が持つ）。
export function useSlideOverlays() {
  const [presenting, setPresenting] = React.useState(false);
  const [exporting, setExporting] = React.useState(false);
  const onPresent = React.useCallback(() => setPresenting(true), []);
  const onExport = React.useCallback(() => setExporting(true), []);
  return { presenting, setPresenting, exporting, setExporting, onPresent, onExport };
}

export function SlideWorkspace({
  doc,
  editable,
  name = "スライド",
  onSelectionChange,
  presenting,
  onPresentingChange,
  exporting,
  onExportingChange,
}: {
  doc: Y.Doc;
  editable: boolean;
  /// ファイル名（拡張子なし・pptx エクスポート名に使う）。
  name?: string;
  /// 要素選択の変化（Task 11.10）。アシスタントパネルが開いていれば自動挿入する。
  onSelectionChange?: (selection: { slideId: string; html: string } | null) => void;
  /// プレゼンモードの表示（[`useSlideOverlays`]）。
  presenting: boolean;
  onPresentingChange: (open: boolean) => void;
  /// エクスポートダイアログの表示。
  exporting: boolean;
  onExportingChange: (open: boolean) => void;
}) {
  const slides = useSlides(doc);
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  // キャンバスの要素選択（選択→AI 指示・Task 11.10）。値は親（アシスタント）が扱うため
  // ここでは保持せず通知だけする。
  const setCanvasSelection = React.useCallback(
    (sel: { slideId: string; html: string } | null) => onSelectionChange?.(sel),
    [onSelectionChange],
  );
  // エディタバンドル未配備（/builtin 404）時の閲覧フォールバック。
  const [editorUnavailable, setEditorUnavailable] = React.useState(false);
  // タッチかつ狭い画面では GrapesJS（マウス前提の砂箱エディタ）を既定にしない。
  const narrowTouch = useMediaQuery("(pointer: coarse) and (max-width: 767.98px)");
  const [forceEdit, setForceEdit] = React.useState(false);
  const stripRef = React.useRef<HTMLDivElement>(null);

  // 選択の解決: 未選択/消滅時は先頭スライドへ寄せる。
  const selected = slides.find((s) => s.id === selectedId) ?? slides[0] ?? null;
  const selectedIndex = selected ? slides.findIndex((s) => s.id === selected.id) : -1;

  const canEdit = editable && !editorUnavailable;
  const editing = canEdit && (!narrowTouch || forceEdit);

  // スライドが無いのにプレゼン/エクスポートを開いた場合は閉じる（空表示で固まらない）。
  React.useEffect(() => {
    if (slides.length === 0 && presenting) onPresentingChange(false);
  }, [slides.length, presenting, onPresentingChange]);

  // 選択中のサムネイルをストリップ内に見せる（‹ › で送ったとき・横スクロールのスマホ）。
  // scrollIntoView は外側（スマホの縦スクロール）まで動かすため、ストリップだけを動かす。
  React.useEffect(() => {
    const strip = stripRef.current;
    const el = strip?.querySelector<HTMLElement>('[aria-current="true"]');
    if (!strip || !el) return;
    const s = strip.getBoundingClientRect();
    const r = el.getBoundingClientRect();
    if (r.left < s.left) strip.scrollLeft += r.left - s.left - 8;
    else if (r.right > s.right) strip.scrollLeft += r.right - s.right + 8;
    if (r.top < s.top) strip.scrollTop += r.top - s.top - 8;
    else if (r.bottom > s.bottom) strip.scrollTop += r.bottom - s.bottom + 8;
  }, [selected?.id]);

  const go = (delta: number) => {
    const next = slides[selectedIndex + delta];
    if (next) setSelectedId(next.id);
  };

  if (slides.length === 0 && !editing) {
    return (
      <EmptyState
        title="スライドがありません"
        description="このドキュメントにはまだスライドがありません。"
        className="m-4"
      />
    );
  }

  return (
    // スマホは全体を 1 本の縦スクロールにする（アシスタントのシートで高さが縮んでも、
    // スライドが潰れずにスクロールで見られる）。ストリップは余白があれば下端に寄せる。
    <div className="flex h-full min-h-0 max-md:flex-col max-md:overflow-y-auto max-md:overscroll-y-contain">
      {/* フィルムストリップ（選択=塗り bg-accent）。スマホではスライドの下に横並び。 */}
      <div
        className={cn(
          "flex w-48 shrink-0 flex-col border-r border-border/60",
          "max-md:order-last max-md:mt-auto max-md:w-full max-md:border-r-0 max-md:border-t",
        )}
        data-testid="slide-filmstrip"
      >
        <div
          ref={stripRef}
          className={cn(
            "min-h-0 flex-1 space-y-3 overflow-y-auto overscroll-y-contain p-3",
            "max-md:flex max-md:flex-none max-md:gap-2 max-md:space-y-0 max-md:overflow-x-auto max-md:overflow-y-hidden max-md:overscroll-x-contain max-md:px-3 max-md:py-2",
          )}
        >
          {slides.map((slide, i) => (
            <button
              key={slide.id}
              type="button"
              onClick={() => setSelectedId(slide.id)}
              aria-label={`スライド ${i + 1} を表示`}
              aria-current={slide.id === selected?.id ? "true" : undefined}
              className={cn(
                "relative block w-full rounded-lg p-1.5 text-left transition-colors max-md:w-28 max-md:shrink-0 max-md:p-1",
                slide.id === selected?.id ? "bg-accent" : "hover:bg-accent/50",
              )}
            >
              <SlideFrame slide={slide} title={`スライド ${i + 1} サムネイル`} />
              <div className="mt-1 px-1 text-xs tabular-nums text-muted-foreground max-md:hidden">
                {i + 1}
              </div>
              <span className="absolute bottom-1.5 left-1.5 rounded bg-background/85 px-1 text-[10.5px] font-medium tabular-nums text-muted-foreground md:hidden">
                {i + 1}
              </span>
            </button>
          ))}
        </div>
        {editing ? (
          <div className="flex items-center gap-1 border-t border-border/60 p-2 max-md:pb-[max(0.5rem,env(safe-area-inset-bottom))]">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="flex-1 max-md:h-10"
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
              className="size-8 max-md:size-10"
              disabled={!selected || selectedIndex <= 0}
              onClick={() => selected && moveSlide(doc, selected.id, -1)}
              aria-label="スライドを前へ移動"
            >
              <ChevronUp className="size-4 max-md:hidden" aria-hidden />
              <ChevronLeft className="size-4 md:hidden" aria-hidden />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8 max-md:size-10"
              disabled={!selected || selectedIndex >= slides.length - 1}
              onClick={() => selected && moveSlide(doc, selected.id, 1)}
              aria-label="スライドを後ろへ移動"
            >
              <ChevronDown className="size-4 max-md:hidden" aria-hidden />
              <ChevronRight className="size-4 md:hidden" aria-hidden />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8 text-destructive hover:text-destructive max-md:size-10"
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
      <div className="flex min-h-0 min-w-0 flex-1 flex-col max-md:flex-none">
        {editorUnavailable ? (
          <p className="px-4 pt-3 text-xs text-muted-foreground">
            エディタバンドルが未配備のため閲覧表示です（管理者に確認してください）
          </p>
        ) : null}
        {canEdit && narrowTouch ? (
          <div className="flex items-center gap-2 px-4 pt-2 text-[13px] text-muted-foreground">
            <span className="min-w-0 flex-1">
              {forceEdit ? "編集中（細かい操作は PC が向いています）" : "この画面幅では閲覧のみです"}
            </span>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-9 shrink-0 gap-1.5"
              onClick={() => setForceEdit((v) => !v)}
              data-testid="slide-force-edit"
            >
              {forceEdit ? <Eye aria-hidden /> : <Pencil aria-hidden />}
              {forceEdit ? "閲覧に戻す" : "それでも編集する"}
            </Button>
          </div>
        ) : null}

        {editing ? (
          <div className="flex min-h-0 flex-1 flex-col gap-3 p-4 max-md:flex-none max-md:p-3">
            <div className="min-h-0 flex-1 max-md:h-[min(60dvh,26rem)] max-md:flex-none">
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
                className="w-full resize-none rounded-lg border border-border/60 bg-card/40 px-3 py-2 text-base outline-none transition-colors focus:border-primary/40 md:text-sm"
                data-testid="slide-notes-input"
              />
            ) : null}
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col items-center gap-4 overflow-y-auto overscroll-y-contain px-6 py-4 max-md:flex-none max-md:overflow-visible max-md:px-3 max-md:py-3 md:justify-center">
            <div className="w-full max-w-[min(72rem,calc((100dvh-14rem)*16/9))]">
              {selected ? (
                <SlideFrame
                  slide={selected}
                  title={`スライド ${selectedIndex + 1}`}
                  className="shadow-md"
                />
              ) : null}
              {/* スマホ: ストリップを指で送らなくても前後へ移れるように。 */}
              <div className="mt-2 flex items-center justify-center gap-3 md:hidden">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="size-11"
                  disabled={selectedIndex <= 0}
                  onClick={() => go(-1)}
                  aria-label="前のスライド"
                >
                  <ChevronLeft className="size-5" aria-hidden />
                </Button>
                <span className="min-w-14 text-center text-sm tabular-nums text-muted-foreground">
                  {selectedIndex + 1} / {slides.length}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="size-11"
                  disabled={selectedIndex >= slides.length - 1}
                  onClick={() => go(1)}
                  aria-label="次のスライド"
                >
                  <ChevronRight className="size-5" aria-hidden />
                </Button>
              </div>
              {selected?.notes ? (
                <div className="mt-4 rounded-lg border border-border/60 bg-card/40 p-4 max-md:mt-2 max-md:p-3">
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

      {presenting && slides.length > 0 ? (
        <PresentMode
          slides={slides}
          initialIndex={Math.max(selectedIndex, 0)}
          onClose={() => onPresentingChange(false)}
        />
      ) : null}
      {exporting ? (
        <ExportDialog
          doc={doc}
          name={name}
          open={exporting}
          onClose={() => onExportingChange(false)}
        />
      ) : null}
    </div>
  );
}
