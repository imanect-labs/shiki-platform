"use client";

import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import {
  ArrowDownToLine,
  ArrowLeft,
  ArrowRight,
  Files,
  FolderOpen,
  GripVertical,
  Loader2,
  Plus,
  Trash2,
  Undo2,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { DrivePicker } from "@/components/chat/drive-picker";
import { PdfPage } from "./pdf-page";
import { assertPdfSource, bindPdfSource, saveBlob } from "@/lib/pdf-excerpts";
import { pdfjsLib } from "@/lib/pdf";
import { mergePdfs, moveBefore, organizePdf } from "@/lib/pdf-page-tools";
import { storageScopeGeneration } from "@/lib/user-storage";
import { versionDownloadUrl, type NodeResponse } from "@/lib/storage";
import {
  checkPdfBudget,
  readPdfResponse,
  PDF_FILE_LIMIT,
  PDF_TOTAL_LIMIT,
} from "@/lib/pdf-input";
import { cn } from "@/lib/utils";

type Source = {
  id: string;
  name: string;
  doc: PDFDocumentProxy;
  owned: boolean;
  bytes?: number;
};
const button =
  "inline-flex min-h-10 min-w-10 items-center justify-center gap-1.5 rounded-lg border px-3 py-2 text-xs hover:bg-accent disabled:pointer-events-none disabled:opacity-35 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
export function PdfPageWorkbench({
  doc,
  name,
  mode,
  onClose,
}: {
  doc: PDFDocumentProxy;
  name: string;
  mode: "organize" | "merge";
  onClose: () => void;
}) {
  const [order, setOrder] = React.useState(() =>
    Array.from({ length: doc.numPages }, (_, i) => i),
  );
  const [history, setHistory] = React.useState<number[][]>([]);
  const [visible, setVisible] = React.useState(100);
  const [sources, setSources] = React.useState<Source[]>([
    { id: "original", name, doc, owned: false },
  ]);
  const sourcesRef = React.useRef(sources);
  const owned = React.useRef(new Set<PDFDocumentProxy>());
  const mounted = React.useRef(true);
  const scope = React.useRef(storageScopeGeneration());
  const [picker, setPicker] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState("");
  const [done, setDone] = React.useState(false);
  const [announcement, setAnnouncement] = React.useState("");
  const [dragged, setDragged] = React.useState<string | null>(null);
  const [over, setOver] = React.useState<string | null>(null);
  const input = React.useRef<HTMLInputElement>(null);
  const loadingOperation = React.useRef(false);
  const transfer = React.useRef<AbortController | null>(null);
  const current = () =>
    mounted.current && scope.current === storageScopeGeneration();
  React.useEffect(() => {
    mounted.current = true;
    const docs = owned.current;
    return () => {
      mounted.current = false;
      transfer.current?.abort();
      docs.forEach((d) => {
        void d.destroy().catch(() => undefined);
      });
      docs.clear();
    };
  }, []);
  const updateSources = (next: Source[]) => {
    sourcesRef.current = next;
    setSources(next);
    setDone(false);
    setAnnouncement("");
  };
  const change = (next: number[], message: string) => {
    if (next === order) return;
    setHistory((h) => [...h.slice(-49), order]);
    setOrder(next);
    setAnnouncement(message);
    setDone(false);
    setError("");
  };
  const totalBytes = async () => {
    let total = 0;
    for (const source of sourcesRef.current) {
      source.bytes ??= (await source.doc.getData()).byteLength;
      total += source.bytes;
    }
    return total;
  };
  const parse = async (bytes: Uint8Array) => {
    if (!current()) throw new Error("ログイン状態が変わりました。");
    const lib = await pdfjsLib();
    if (!current()) throw new Error("ログイン状態が変わりました。");
    const task = lib.getDocument({ data: bytes });
    try {
      return await task.promise;
    } catch (e) {
      await task.destroy();
      throw new Error(
        e instanceof Error && e.name === "PasswordException"
          ? "パスワードで保護されたPDFは追加できません。保護を解除したファイルを選んでください。"
          : "PDFを開けませんでした。ファイルが壊れていないか確認してください。",
      );
    }
  };
  const add = async (get: () => Promise<Source>) => {
    if (sourcesRef.current.length >= 20)
      throw new Error("一度に結合できるPDFは20ファイルまでです。");
    const source = await get();
    if (!current()) {
      if (source.owned) void source.doc.destroy().catch(() => undefined);
      return;
    }
    if (source.owned) owned.current.add(source.doc);
    if (
      sourcesRef.current.reduce((n, s) => n + s.doc.numPages, 0) +
        source.doc.numPages >
      1000
    ) {
      if (source.owned) {
        owned.current.delete(source.doc);
        void source.doc.destroy().catch(() => undefined);
      }
      throw new Error("一度に結合できるのは合計1,000ページまでです。");
    }
    updateSources([...sourcesRef.current, source]);
    setAnnouncement(`${source.name} を追加しました`);
  };
  const localFiles = async (files: File[]) => {
    if (loadingOperation.current) return;
    loadingOperation.current = true;
    setLoading(true);
    setError("");
    setDone(false);
    setAnnouncement("");
    try {
      for (const file of files) {
        if (!current()) return;
        checkPdfBudget(await totalBytes(), file.size);
        if (!current()) return;
        if (!/\.pdf$/i.test(file.name) && file.type !== "application/pdf")
          throw new Error("PDFファイルを選んでください。");
        await add(async () => {
          const doc = await parse(new Uint8Array(await file.arrayBuffer()));
          return {
            id: crypto.randomUUID(),
            name: file.name,
            doc,
            owned: true,
            bytes: file.size,
          };
        });
      }
    } catch (e) {
      if (current())
        setError(e instanceof Error ? e.message : "PDFを開けませんでした。");
    } finally {
      loadingOperation.current = false;
      if (current()) setLoading(false);
    }
  };
  const driveFile = async (node: NodeResponse) => {
    if (loadingOperation.current) return;
    loadingOperation.current = true;
    setPicker(false);
    setLoading(true);
    setError("");
    setDone(false);
    setAnnouncement("");
    try {
      await add(async () => {
        const total = await totalBytes();
        checkPdfBudget(total, node.size_bytes ?? 0);
        if (!current()) throw new Error("ログイン状態が変わりました。");
        const ticket = await versionDownloadUrl(node.id, node.version);
        if (!current()) throw new Error("ログイン状態が変わりました。");
        transfer.current = new AbortController();
        const bytes = await readPdfResponse(
          await fetch(ticket.url, { signal: transfer.current.signal }),
          Math.min(PDF_FILE_LIMIT, PDF_TOTAL_LIMIT - total),
        );
        if (!current()) throw new Error("ログイン状態が変わりました。");
        const size = bytes.byteLength;
        const selected = await parse(bytes);
        if (current()) bindPdfSource(selected, node.id, node.version);
        return {
          id: crypto.randomUUID(),
          name: node.name,
          doc: selected,
          owned: true,
          bytes: size,
        };
      });
    } catch (e) {
      if (current())
        setError(e instanceof Error ? e.message : "PDFを開けませんでした。");
    } finally {
      loadingOperation.current = false;
      if (current()) setLoading(false);
    }
  };
  const save = async () => {
    setBusy(true);
    setError("");
    setDone(false);
    try {
      assertPdfSource(doc);
      const bytes =
        mode === "organize"
          ? await organizePdf(doc, [...order])
          : await mergePdfs(sources.map((s) => s.doc));
      if (!current()) return;
      assertPdfSource(doc);
      saveBlob(
        new Blob([new Uint8Array(bytes)], { type: "application/pdf" }),
        `${name.replace(/\.pdf$/i, "")}-${mode === "organize" ? "ページ編集" : "結合"}.pdf`,
      );
      setDone(true);
    } catch (e) {
      if (current())
        setError(
          e instanceof Error ? e.message : "PDFを作成できませんでした。",
        );
    } finally {
      if (current()) setBusy(false);
    }
  };
  const blocked = busy || loading;
  const dropProps = (key: string, drop: () => void) => ({
    draggable: !blocked,
    onDragStart: (e: React.DragEvent) => {
      e.dataTransfer.setData("text/plain", key);
      e.dataTransfer.effectAllowed = "move";
      setDragged(key);
    },
    onDragOver: (e: React.DragEvent) => {
      if (dragged && !blocked) {
        e.preventDefault();
        setOver(key);
      }
    },
    onDrop: (e: React.DragEvent) => {
      e.preventDefault();
      if (dragged && !blocked) drop();
      setDragged(null);
      setOver(null);
    },
    onDragEnd: () => {
      setDragged(null);
      setOver(null);
    },
  });
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="flex max-h-[92dvh] w-[min(980px,96vw)] max-w-[980px] flex-col gap-0 overflow-hidden p-0">
        <div className="shiki-dash-bottom rule-soft px-5 py-5 pr-12 sm:px-7">
          <p className="mb-2 text-[10px] font-semibold tracking-[.2em] text-muted-foreground">
            PDF WORKSPACE
          </p>
          <DialogTitle className="text-xl">
            {mode === "organize"
              ? "ページの並べ替え・削除"
              : "PDFの結合"}
          </DialogTitle>
          <DialogDescription className="mt-2 text-xs leading-relaxed">
            {mode === "organize"
              ? "ドラッグまたは矢印ボタンで並べ替えられます。削除したページは元に戻せます。"
              : "一覧の上から順にPDFを結合します。ドラッグまたは矢印ボタンで順序を変更できます。"}
          </DialogDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-b bg-muted/30 px-5 py-3 sm:px-7">
          <span className="mr-auto text-xs font-medium">
            {mode === "organize"
              ? `${order.length} / ${doc.numPages} ページ`
              : `${sources.length} ファイル · ${sources.reduce((n, s) => n + s.doc.numPages, 0)} ページ`}
          </span>
          {mode === "organize" ? (
            <>
              <button
                className={button}
                disabled={blocked || !history.length}
                onClick={() => {
                  const previous = history[history.length - 1];
                  if (previous) {
                    setOrder(previous);
                    setHistory((h) => h.slice(0, -1));
                    setDone(false);
                    setAnnouncement("直前の操作を元に戻しました");
                  }
                }}
              >
                <Undo2 className="size-3.5" />
                元に戻す
              </button>
              <button
                className={button}
                disabled={blocked || !history.length}
                onClick={() =>
                  change(
                    Array.from({ length: doc.numPages }, (_, i) => i),
                    "元のページ順へ戻しました",
                  )
                }
              >
                最初の状態へ
              </button>
            </>
          ) : (
            <>
              <input
                ref={input}
                type="file"
                accept="application/pdf,.pdf"
                multiple
                className="sr-only"
                aria-label="結合するPDFを追加"
                onChange={(e) => {
                  const files = Array.from(e.target.files ?? []);
                  e.target.value = "";
                  void localFiles(files);
                }}
              />
              <button
                className={button}
                disabled={blocked}
                onClick={() => input.current?.click()}
              >
                <Plus className="size-3.5" />
                端末から追加
              </button>
              <button
                className={button}
                disabled={blocked}
                onClick={() => setPicker(true)}
              >
                <FolderOpen className="size-3.5" />
                ドライブから
              </button>
            </>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-auto bg-muted/20 p-5 sm:p-7">
          {mode === "organize" ? (
            <div
              className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4"
              data-testid="pdf-page-order"
            >
              {order.slice(0, visible).map((page, i) => (
                <article
                  key={page}
                  data-testid="pdf-organize-page"
                  data-source-page={page + 1}
                  {...dropProps(String(page), () =>
                    change(
                      moveBefore(order, Number(dragged), page),
                      `ページ${Number(dragged) + 1}を${i + 1}番目へ移動しました`,
                    ),
                  )}
                  className={cn(
                    "group overflow-hidden rounded-xl border bg-background shadow-sm transition-shadow",
                    over === String(page)
                      ? "ring-2 ring-primary"
                      : "hover:shadow-md",
                    dragged === String(page) && "opacity-50",
                  )}
                >
                  <div className="flex items-center gap-2 border-b px-3 py-2 text-xs">
                    <GripVertical className="size-3.5 cursor-grab text-muted-foreground" />
                    <span className="font-semibold">{i + 1}</span>
                    <span className="ml-auto text-[10px] text-muted-foreground">
                      元の p.{page + 1}
                    </span>
                  </div>
                  <div className="flex h-[170px] items-center justify-center overflow-hidden bg-muted/20 p-3">
                    <PdfPage doc={doc} pageNumber={page + 1} width={105} />
                  </div>
                  <div className="flex items-center justify-between border-t p-1.5">
                    <button
                      className={button}
                      disabled={blocked || i === 0}
                      aria-label={`ページ${page + 1}を前へ`}
                      onClick={() =>
                        change(
                          moveBefore(order, page, order[i - 1]),
                          `ページ${page + 1}を前へ移動しました`,
                        )
                      }
                    >
                      <ArrowLeft className="size-3.5" />
                    </button>
                    <button
                      className="min-h-10 min-w-10 rounded-lg p-2 text-muted-foreground hover:bg-destructive/10 hover:text-destructive disabled:opacity-35"
                      disabled={blocked}
                      aria-label={`ページ${page + 1}を削除`}
                      onClick={() =>
                        change(
                          order.filter((p) => p !== page),
                          `ページ${page + 1}を削除しました`,
                        )
                      }
                    >
                      <Trash2 className="size-3.5" />
                    </button>
                    <button
                      className={button}
                      disabled={blocked || i === order.length - 1}
                      aria-label={`ページ${page + 1}を後ろへ`}
                      onClick={() =>
                        change(
                          moveBefore(order, page, order[i + 1]),
                          `ページ${page + 1}を後ろへ移動しました`,
                        )
                      }
                    >
                      <ArrowRight className="size-3.5" />
                    </button>
                  </div>
                </article>
              ))}
              {!order.length && (
                <p className="col-span-full py-12 text-center text-sm text-muted-foreground">
                  ページがありません。「元に戻す」でページを戻してください。
                </p>
              )}
              {order.length > visible && (
                <button
                  className={button}
                  onClick={() => setVisible((n) => n + 100)}
                >
                  続きを表示（残り{order.length - visible}ページ）
                </button>
              )}
            </div>
          ) : (
            <div className="space-y-3" data-testid="pdf-merge-sources">
              {sources.map((source, i) => (
                <article
                  key={source.id}
                  data-testid="pdf-merge-source"
                  {...dropProps(source.id, () => {
                    const moved = moveBefore(
                      sourcesRef.current,
                      sourcesRef.current.find((s) => s.id === dragged)!,
                      source,
                    );
                    updateSources(moved);
                  })}
                  className={cn(
                    "flex items-center gap-3 rounded-xl border bg-background p-3 shadow-sm",
                    over === source.id && "ring-2 ring-primary",
                    dragged === source.id && "opacity-50",
                  )}
                >
                  <GripVertical className="size-4 shrink-0 cursor-grab text-muted-foreground" />
                  <div className="flex h-20 w-14 shrink-0 items-center overflow-hidden rounded border bg-white">
                    <PdfPage doc={source.doc} pageNumber={1} width={54} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <span className="text-[10px] font-semibold tracking-wider text-muted-foreground">
                      {String(i + 1).padStart(2, "0")}
                    </span>
                    <p
                      className="truncate text-sm font-medium"
                      title={source.name}
                    >
                      {source.name}
                    </p>
                    <p className="mt-1 text-xs text-muted-foreground">
                      {source.doc.numPages} ページ
                    </p>
                  </div>
                  <div className="flex flex-col gap-1 sm:flex-row">
                    <button
                      className={button}
                      disabled={blocked || i === 0}
                      aria-label={`${source.name}を前へ`}
                      onClick={() =>
                        updateSources(
                          moveBefore(sources, source, sources[i - 1]),
                        )
                      }
                    >
                      <ArrowLeft className="size-3.5" />
                    </button>
                    <button
                      className={button}
                      disabled={blocked || i === sources.length - 1}
                      aria-label={`${source.name}を後ろへ`}
                      onClick={() =>
                        updateSources(
                          moveBefore(sources, source, sources[i + 1]),
                        )
                      }
                    >
                      <ArrowRight className="size-3.5" />
                    </button>
                    <button
                      className={button}
                      disabled={blocked}
                      aria-label={`${source.name}を結合から外す`}
                      onClick={() => {
                        updateSources(sources.filter((s) => s !== source));
                        if (source.owned) {
                          owned.current.delete(source.doc);
                          void source.doc.destroy().catch(() => undefined);
                        }
                      }}
                    >
                      <Trash2 className="size-3.5" />
                    </button>
                  </div>
                </article>
              ))}
              {sources.length < 2 && (
                <div className="rounded-xl border border-dashed p-8 text-center text-muted-foreground">
                  <Files className="mx-auto mb-3 size-7" />
                  <p className="text-xs">結合するPDFを追加してください</p>
                </div>
              )}
            </div>
          )}
        </div>
        <div className="shiki-dash-top rule-soft bg-background p-5 sm:px-7">
          <p role="status" className="mb-2 min-h-4 text-xs text-primary">
            {loading
              ? "PDFを読み込み中…"
              : done
                ? "PDFを保存しました。"
                : announcement}
          </p>
          {error && (
            <p role="alert" className="mb-3 text-xs text-destructive">
              {error}
            </p>
          )}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="max-w-lg text-[11px] leading-relaxed text-muted-foreground">
              原本は変更されません。
              {mode === "merge" || order.length !== doc.numPages
                ? "ページ内容を新しいPDFへ保存します。入力フォーム・しおりは引き継ぎません。"
                : "並べ替えでは入力フォーム・しおりを保持します。"}
            </p>
            <button
              type="button"
              disabled={
                blocked ||
                (mode === "organize" ? !order.length : sources.length < 2)
              }
              onClick={() => void save()}
              className="flex h-10 items-center justify-center gap-2 rounded-lg bg-primary px-5 text-sm font-medium text-primary-foreground hover:opacity-90 disabled:opacity-40"
            >
              {busy ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <ArrowDownToLine className="size-4" />
              )}
              {busy ? "PDFを作成中…" : "PDF を保存"}
            </button>
          </div>
        </div>
        <DrivePicker
          open={picker}
          onOpenChange={setPicker}
          onSelect={(node) => void driveFile(node)}
          title="結合するPDFを選択"
          description="選んだPDFを結合リストへ追加します。"
          accept={(node) =>
            node.content_type === "application/pdf" || /\.pdf$/i.test(node.name)
          }
        />
      </DialogContent>
    </Dialog>
  );
}
