"use client";

import * as React from "react";
import Image from "next/image";
import {
  ArrowDownToLine,
  ArrowUpRight,
  Check,
  Copy,
  FileSearch,
  ImageDown,
  Loader2,
  MessageSquare,
  ScanText,
  Table2,
  X,
} from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import {
  regionPng,
  saveBlob,
  tableCsv,
  type PdfExcerpt,
} from "@/lib/pdf-excerpts";

export function PdfExcerptPanel({
  doc,
  selections,
  busy,
  error,
  onRemove,
  onClear,
  onJump,
  onAttach,
  onRetry,
  includeImages,
  onIncludeImages,
}: {
  doc: PDFDocumentProxy;
  selections: PdfExcerpt[];
  busy: boolean;
  error: string;
  includeImages: boolean;
  onIncludeImages: (value: boolean) => void;
  onRemove: (id: string) => void;
  onClear: () => void;
  onJump: (page: number) => void;
  onAttach: (destination: "panel" | "new") => void;
  onRetry: () => void;
}) {
  const [feedback, setFeedback] = React.useState("");
  const [actionError, setActionError] = React.useState("");
  const chars = Array.from(selections.map((s) => s.text).join("\n\n")).length;
  const tables = selections.flatMap((s) => s.tables);
  const canAttach = selections.length > 0 && chars <= 7600 && !busy;
  const act = async (fn: () => Promise<void>) => {
    setActionError("");
    try {
      await fn();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : "操作に失敗しました");
    }
  };
  return (
    <div className="flex h-full flex-col" data-testid="pdf-excerpt-panel">
      <div className="shiki-dash-bottom rule-soft p-4">
        <div className="mb-2 flex items-center justify-between">
          <span className="text-[10px] font-semibold tracking-[0.18em] text-muted-foreground">
            EXCERPTS
          </span>
          <span className="rounded-full bg-primary/10 px-2 py-0.5 text-[10px] font-medium text-primary">
            {selections.length} / 20
          </span>
        </div>
        <h2 className="text-base font-semibold">選択した抜粋</h2>
        <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
          内容を確認して、チャットへ添付できます。
        </p>
      </div>
      <div
        className="min-h-0 flex-1 space-y-3 overflow-auto p-4"
        aria-busy={busy}
      >
        {selections.length === 0 && !busy && (
          <div className="flex flex-col items-center px-4 py-12 text-center">
            <div className="mb-4 flex size-14 items-center justify-center rounded-2xl border border-primary/20 bg-primary/5">
              <ScanText className="size-6 text-primary" strokeWidth={1.5} />
            </div>
            <p className="text-sm font-medium">最初の範囲を選択</p>
            <p className="mt-2 text-xs leading-6 text-muted-foreground">
              選択した範囲の内容がここに表示されます。
            </p>
            <span className="mt-5 rounded-md border px-2 py-1 text-[10px] text-muted-foreground">
              矢印で位置 · Shift＋矢印でサイズ · Enterで追加
            </span>
          </div>
        )}
        {selections.map((s, i) => (
          <article
            key={s.id}
            data-testid="pdf-excerpt-card"
            className="overflow-hidden rounded-xl border border-border/80 bg-background shadow-sm"
          >
            <div className="flex items-center gap-2 shiki-dash-bottom rule-soft bg-muted/25 px-3 py-2">
              <span className="flex size-5 items-center justify-center rounded-full bg-primary text-[10px] font-semibold text-primary-foreground">
                {i + 1}
              </span>
              <button
                type="button"
                onClick={() => onJump(s.page)}
                className="flex items-center gap-1 text-xs font-medium hover:text-primary"
              >
                p.{s.page}
                <ArrowUpRight className="size-3" />
              </button>
              <span className="ml-auto text-[10px] text-muted-foreground">
                {s.precision === "line" ? "行" : s.precision === "cell" ? "表セル" : s.tables.length ? "表を含む" : "テキスト"}
              </span>
              <button
                type="button"
                aria-label={`抜粋${i + 1}を削除`}
                onClick={() => onRemove(s.id)}
                className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <X className="size-3.5" />
              </button>
            </div>
            <ExcerptThumbnail doc={doc} excerpt={s} />
            <details className="group px-3 py-3">
              <summary className="cursor-pointer list-none text-xs leading-relaxed">
                <span className="line-clamp-2 whitespace-pre-wrap group-open:hidden">
                  {s.text || "文字が見つかりません。画像として添付されます。"}
                </span>
                <span className="hidden text-[10px] text-primary group-open:inline">
                  プレビューを閉じる
                </span>
              </summary>
              <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap break-words font-sans text-xs leading-relaxed">
                {s.text}
              </pre>
            </details>
            <div className="flex items-center gap-3 px-3 pb-3">
              <button
                type="button"
                onClick={() =>
                  void act(async () => {
                    await navigator.clipboard.writeText(s.text);
                    setFeedback(`抜粋${i + 1}をコピーしました`);
                  })
                }
                disabled={!s.text}
                className="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground disabled:opacity-40"
              >
                <Copy className="size-3" />
                コピー
              </button>
              <button
                type="button"
                onClick={() =>
                  void act(async () =>
                    saveBlob(await regionPng(doc, s), `抜粋-p${s.page}.png`),
                  )
                }
                className="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground"
              >
                <ImageDown className="size-3" />
                画像を保存
              </button>
            </div>
          </article>
        ))}
        {busy && (
          <div
            role="status"
            className="flex items-center gap-2 rounded-xl border border-dashed p-4 text-xs text-muted-foreground"
          >
            <Loader2 className="size-4 animate-spin" />
            保存済みの解析結果から抽出中…
          </div>
        )}
        {error && (
          <div
            role="alert"
            className="rounded-xl border border-destructive/20 bg-destructive/5 p-3 text-xs text-destructive"
          >
            <p>{error}</p>
            <button
              type="button"
              onClick={onRetry}
              className="mt-2 underline underline-offset-2"
            >
              解析結果を再取得
            </button>
          </div>
        )}
      </div>
      <div className="space-y-2 shiki-dash-top rule-soft bg-background p-4">
        {selections.length > 0 && (
          <>
            <div className="flex items-center justify-between text-[11px] text-muted-foreground">
              <span>
                {chars.toLocaleString()} 文字
                {tables.length > 0 ? ` · 表 ${tables.length} 件` : ""}
              </span>
              <button
                type="button"
                onClick={onClear}
                className="hover:text-foreground"
              >
                すべて解除
              </button>
            </div>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() =>
                  void act(async () => {
                    await navigator.clipboard.writeText(
                      selections.map((s) => s.text).join("\n\n"),
                    );
                    setFeedback("すべての抜粋をコピーしました");
                  })
                }
                className="flex flex-1 items-center justify-center gap-1.5 rounded-lg border py-2 text-xs hover:bg-accent"
              >
                <Copy className="size-3.5" />
                まとめてコピー
              </button>
              <button
                type="button"
                disabled={!tables.length}
                onClick={() =>
                  tables.forEach((t, i) =>
                    saveBlob(
                      new Blob([tableCsv(t)], {
                        type: "text/csv;charset=utf-8",
                      }),
                      `表-${i + 1}.csv`,
                    ),
                  )
                }
                className="flex items-center gap-1.5 rounded-lg border px-3 py-2 text-xs hover:bg-accent disabled:opacity-40"
              >
                <Table2 className="size-3.5" />
                <ArrowDownToLine className="size-3" />
                CSV
              </button>
            </div>
          </>
        )}
        <div className="flex items-start gap-2 text-[10px] leading-relaxed text-muted-foreground">
          <FileSearch className="mt-0.5 size-3.5 shrink-0" />
          <p>出典のページ・版・位置も一緒に添付</p>
        </div>
        {chars > 7600 && (
          <p role="alert" className="text-xs text-destructive">
            添付上限を超えています。範囲を減らしてください。
          </p>
        )}
        {selections.length > 0 && (
          <label className="flex cursor-pointer items-start gap-2 text-[11px] leading-relaxed text-muted-foreground">
            <input
              type="checkbox"
              checked={includeImages}
              onChange={(e) => onIncludeImages(e.target.checked)}
              className="mt-0.5 accent-primary"
            />
            画像も添付（ドライブに保存）
          </label>
        )}
        <div className="grid grid-cols-[1.2fr_1fr] gap-2">
          <button
            type="button"
            aria-label="アシスタントに添付"
            disabled={!canAttach}
            onClick={() => onAttach("panel")}
            className="flex h-10 items-center justify-center gap-1.5 rounded-lg bg-primary text-xs font-medium text-primary-foreground transition-opacity hover:opacity-90 disabled:opacity-40"
          >
            <MessageSquare className="size-3.5" />
            アシスタントへ
          </button>
          <button
            type="button"
            aria-label="新規チャットに添付"
            disabled={!canAttach}
            onClick={() => onAttach("new")}
            className="flex h-10 items-center justify-center gap-1 rounded-lg border text-xs font-medium hover:bg-accent disabled:opacity-40"
          >
            新規チャットへ
            <ArrowUpRight className="size-3.5" />
          </button>
        </div>
        {feedback && (
          <p
            role="status"
            className="flex items-center gap-1 text-[10px] text-primary"
          >
            <Check className="size-3" />
            {feedback}
          </p>
        )}
        {actionError && (
          <p role="alert" className="text-xs text-destructive">
            {actionError}
          </p>
        )}
      </div>
    </div>
  );
}

function ExcerptThumbnail({
  doc,
  excerpt,
}: {
  doc: PDFDocumentProxy;
  excerpt: PdfExcerpt;
}) {
  const [url, setUrl] = React.useState<string | null>(null);
  React.useEffect(() => {
    let active = true;
    let objectUrl: string | null = null;
    regionPng(doc, excerpt)
      .then((blob) => {
        if (!active) return;
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      })
      .catch(() => undefined);
    return () => {
      active = false;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [doc, excerpt]);
  return url ? (
    <div className="border-b border-border/40 bg-white p-3">
      <Image
        width={320}
        height={112}
        unoptimized
        src={url}
        alt={`p.${excerpt.page} の選択範囲`}
        className="max-h-20 w-full object-contain object-left"
      />
    </div>
  ) : null;
}
