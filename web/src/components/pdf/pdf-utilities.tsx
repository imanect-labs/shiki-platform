"use client";

import * as React from "react";
import {
  ArrowDownToLine,
  FileOutput,
  Loader2,
  RotateCw,
  Scissors,
} from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { saveBlob, assertPdfSource } from "@/lib/pdf-excerpts";
import { cn } from "@/lib/utils";

export function parsePageRange(input: string, count: number): number[] {
  const pages: number[] = [];
  for (const part of input.split(/[,、]/)) {
    const m = part.trim().match(/^(\d+)(?:\s*[-–]\s*(\d+))?$/);
    if (!m) throw new Error("ページは「1-3, 5」のように入力してください。");
    const a = Number(m[1]),
      b = Number(m[2] ?? m[1]);
    if (a < 1 || b < a || b > count)
      throw new Error(`1〜${count} のページを指定してください。`);
    for (let p = a; p <= b; p++) if (!pages.includes(p - 1)) pages.push(p - 1);
  }
  return pages;
}
export function PdfUtilities({
  doc,
  name,
  current,
}: {
  doc: PDFDocumentProxy;
  name: string;
  current: number;
}) {
  const [tool, setTool] = React.useState<"extract" | "rotate">("extract");
  const [range, setRange] = React.useState(`1-${doc.numPages}`);
  const [degrees, setDegrees] = React.useState(90);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [done, setDone] = React.useState(false);
  const run = async () => {
    setBusy(true);
    setError("");
    setDone(false);
    try {
      assertPdfSource(doc);
      const pages = parsePageRange(range, doc.numPages);
      const { PDFDocument, degrees: angle } = await import("pdf-lib");
      const source = await PDFDocument.load(await doc.getData());
      const output = await PDFDocument.create();
      if (tool === "extract") {
        for (const p of await output.copyPages(source, pages))
          output.addPage(p);
      } else {
        const all = await output.copyPages(source, source.getPageIndices());
        for (let i = 0; i < all.length; i++) {
          if (pages.includes(i))
            all[i].setRotation(
              angle((all[i].getRotation().angle + degrees) % 360),
            );
          output.addPage(all[i]);
        }
      }
      const bytes = await output.save();
      assertPdfSource(doc);
      saveBlob(
        new Blob([new Uint8Array(bytes)], { type: "application/pdf" }),
        `${name}-${tool === "extract" ? "抜粋" : "回転"}.pdf`,
      );
      setDone(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : "PDF を作成できませんでした");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex h-full flex-col" data-testid="pdf-utilities">
      <div className="border-b border-border/60 p-5">
        <div className="mb-2 text-[10px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">
          PDF TOOLS
        </div>
        <h2 className="text-base font-semibold">必要な形に、整える</h2>
        <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
          ページを選んで、新しい PDF として保存。原本はそのまま残ります。
        </p>
      </div>
      <div className="flex-1 space-y-6 overflow-auto p-5">
        <div className="grid grid-cols-2 gap-2">
          {(
            [
              {
                key: "extract",
                icon: Scissors,
                label: "ページを抽出",
                sub: "必要なページだけ",
              },
              {
                key: "rotate",
                icon: RotateCw,
                label: "ページを回転",
                sub: "向きを整える",
              },
            ] as const
          ).map(({ key, icon: Icon, label, sub }) => (
            <button
              type="button"
              key={key}
              aria-pressed={tool === key}
              onClick={() => {
                setTool(key);
                setDone(false);
              }}
              className={cn(
                "rounded-xl border p-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                tool === key
                  ? "border-primary bg-primary/5"
                  : "border-border hover:bg-accent",
              )}
            >
              <Icon className="mb-3 size-5 text-primary" />
              <span className="block text-xs font-medium">{label}</span>
              <span className="mt-1 block text-[10px] text-muted-foreground">
                {sub}
              </span>
            </button>
          ))}
        </div>
        <div>
          <label htmlFor="pdf-tool-pages" className="text-xs font-medium">
            対象のページ
          </label>
          <input
            id="pdf-tool-pages"
            value={range}
            onChange={(e) => {
              setRange(e.target.value);
              setDone(false);
            }}
            className="mt-2 h-10 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
            placeholder="例: 1-3, 5"
          />
          <div className="mt-2 flex gap-3 text-[11px]">
            <button
              type="button"
              className="text-primary hover:underline"
              onClick={() => setRange(`1-${doc.numPages}`)}
            >
              すべて
            </button>
            <button
              type="button"
              className="text-primary hover:underline"
              onClick={() => setRange(String(current))}
            >
              現在のページ
            </button>
            <span className="ml-auto text-muted-foreground">
              全 {doc.numPages} ページ
            </span>
          </div>
        </div>
        {tool === "rotate" && (
          <fieldset>
            <legend className="mb-2 text-xs font-medium">時計回りに回転</legend>
            <div className="flex rounded-lg border p-1">
              {[90, 180, 270].map((d) => (
                <button
                  key={d}
                  type="button"
                  aria-pressed={degrees === d}
                  onClick={() => setDegrees(d)}
                  className={cn(
                    "flex-1 rounded-md py-2 text-xs",
                    degrees === d
                      ? "bg-primary text-primary-foreground"
                      : "hover:bg-accent",
                  )}
                >
                  {d}°
                </button>
              ))}
            </div>
          </fieldset>
        )}
        <div className="rounded-xl border border-dashed border-border bg-muted/30 p-4">
          <FileOutput className="mb-2 size-5 text-muted-foreground" />
          <p className="text-xs font-medium">
            {tool === "extract"
              ? "選んだページを1つの PDF に"
              : "指定ページの向きを変更"}
          </p>
          <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
            処理はブラウザ内で行います。出力した PDF
            はダウンロードして確認できます。
          </p>
        </div>
      </div>
      <div className="border-t border-border/60 p-4">
        {error && (
          <p role="alert" className="mb-3 text-xs text-destructive">
            {error}
          </p>
        )}
        {done && (
          <p role="status" className="mb-3 text-xs text-primary">
            PDF を作成しました。
          </p>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={() => void run()}
          className="flex h-10 w-full items-center justify-center gap-2 rounded-lg bg-primary text-sm font-medium text-primary-foreground transition-opacity hover:opacity-90 disabled:opacity-50"
        >
          {busy ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <ArrowDownToLine className="size-4" />
          )}
          {busy ? "PDF を作成中…" : "PDF を保存"}
        </button>
      </div>
    </div>
  );
}
