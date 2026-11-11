"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  BookOpen,
  ChevronRight,
  Loader2,
  MessageSquare,
  MousePointer2,
  ScanText,
  RefreshCw,
  SlidersHorizontal,
} from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import dynamic from "next/dynamic";
import { PdfExcerptPanel } from "./pdf-excerpt-panel";
import { PdfUtilities } from "./pdf-utilities";

import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { queuePdfAttachments, clearPdfAttachmentsForTarget } from "@/lib/pdf-attachment-handoff";
import { uploadNewFile } from "@/lib/storage";
import type { Attachment } from "@/lib/chat-api";
import {
  storageScopeGeneration,
  subscribeStorageScope,
} from "@/lib/user-storage";
import {
  setPendingSelection,
  clearPendingSelectionForTarget,
} from "@/lib/selection-context";
import {
  excerptContext,
  extractRegion,
  resetPdfBlocks,
  resetDocumentBlocks,
  regionPng,
  assertPdfSource,
  type PdfExcerpt,
  type PdfRect,
} from "@/lib/pdf-excerpts";
import { resetPageText, resetDocumentText } from "@/lib/pdf-text";
import { useMediaQuery } from "@/hooks/use-media-query";
import { useStorageScopeReady } from "@/lib/user-storage";
import { cn } from "@/lib/utils";

const PdfAssistant = dynamic(
  () => import("./pdf-assistant").then((m) => m.PdfAssistant),
  {
    loading: () => (
      <div role="status" className="p-5 text-xs text-muted-foreground">
        チャットを準備しています…
      </div>
    ),
  },
);

type Mode = "read" | "select" | "tools";
export function usePdfWorkspace({
  id,
  version,
  doc,
  name,
  current,
  jumpTo,
}: {
  id: string;
  version: number | null;
  doc: PDFDocumentProxy | null;
  name: string;
  current: number;
  jumpTo: (page: number) => void;
}) {
  const router = useRouter();
  const [mode, setMode] = React.useState<Mode>("read");
  const [selections, setSelections] = React.useState<PdfExcerpt[]>([]);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [assistantOpen, setAssistantOpen] = React.useState(false);
  const [mobilePanel, setMobilePanel] = React.useState(false);
  const [includeImages, setIncludeImages] = React.useState(false);
  const [attaching, setAttaching] = React.useState(false);
  const [textRevision, setTextRevision] = React.useState(0);
  const generation = React.useRef(0);
  const attachmentGeneration = React.useRef(0);
  const pendingCount = React.useRef(0);
  const replacements = React.useRef(new Map<string, symbol>());
  const invalidate = React.useCallback(() => {
    generation.current++;
    attachmentGeneration.current++;
  }, []);
  const lastRegion = React.useRef<{
    page: number;
    bbox: PdfRect;
    id?: string;
  } | null>(null);
  const scopeReady = useStorageScopeReady();
  const authGeneration = React.useSyncExternalStore(
    subscribeStorageScope,
    storageScopeGeneration,
    () => 0,
  );
  const isMobile = useMediaQuery("(max-width: 767px)");
  React.useEffect(() => {
    invalidate();
    pendingCount.current = 0;
    lastRegion.current = null;
    replacements.current.clear();
    setSelections([]);
    setBusy(false);
    setAttaching(false);
    setError("");
    setMode("read");
    setAssistantOpen(false);
    return () => {
      invalidate();
      clearPendingSelectionForTarget(`pdf:${id}`);
      clearPdfAttachmentsForTarget(`pdf:${id}`);
    };
  }, [id, version, authGeneration, invalidate]);
  const onSelect = React.useCallback(
    async (page: number, bbox: PdfRect, replaceId?: string) => {
      if (!doc) return;
      if (
        selections.length + pendingCount.current - (replaceId ? 1 : 0) >=
        20
      ) {
        setError("一度に選べる範囲は20件までです。");
        return;
      }
      if (replaceId && replacements.current.has(replaceId)) return;
      const replacementToken = Symbol();
      if (replaceId) replacements.current.set(replaceId, replacementToken);
      const request = { page, bbox, id: replaceId };
      lastRegion.current = request;
      const at = generation.current;
      const scope = storageScopeGeneration();
      pendingCount.current++;
      setBusy(true);
      setError("");
      try {
        const result = await extractRegion(doc, page, bbox);
        if (
          at !== generation.current ||
          scope !== storageScopeGeneration() ||
          (replaceId &&
            replacements.current.get(replaceId) !== replacementToken)
        )
          return;
        setSelections((prev) =>
          replaceId && !prev.some((s) => s.id === replaceId)
            ? prev
            : [...prev.filter((s) => s.id !== replaceId), result].sort(
                (a, b) =>
                  a.page - b.page ||
                  b.bbox[3] - a.bbox[3] ||
                  a.bbox[0] - b.bbox[0],
              ),
        );
        if (lastRegion.current === request)
          lastRegion.current = { page, bbox, id: result.id };
        if (!result.text) {
          lastRegion.current = { page, bbox, id: result.id };
          setError(
            "この範囲の文字がまだ取得できません。解析完了後に再取得するか、画像として保存してください。",
          );
        }
      } catch (e) {
        if (
          at === generation.current &&
          scope === storageScopeGeneration() &&
          (!replaceId ||
            replacements.current.get(replaceId) === replacementToken)
        ) {
          lastRegion.current = request;
          setError(
            e instanceof Error ? e.message : "解析結果を取得できませんでした",
          );
        }
      } finally {
        if (
          replaceId &&
          replacements.current.get(replaceId) === replacementToken
        )
          replacements.current.delete(replaceId);
        if (at === generation.current) {
          pendingCount.current--;
          setBusy(pendingCount.current > 0);
        }
      }
    },
    [doc, selections.length],
  );
  const attach = async (destination: "panel" | "new") => {
    if (version == null || !doc) return;
    try {
      assertPdfSource(doc);
    } catch (e) {
      setError(e instanceof Error ? e.message : "PDF を開き直してください");
      return;
    }
    const context = excerptContext(id, name, version, selections);
    // サーバの切り詰めに任せない。抜粋や出典が欠ける添付は、ユーザーに範囲を減らしてもらう。
    if (
      Array.from(context.excerpt).length > 8000 ||
      JSON.stringify(context.locator).length > 4096
    ) {
      setError("添付上限を超えています。範囲を減らしてください。");
      return;
    }
    const at = generation.current;
    const scope = storageScopeGeneration();
    const attachmentAt = ++attachmentGeneration.current;
    const isCurrent = () => at === generation.current && scope === storageScopeGeneration() && attachmentAt === attachmentGeneration.current;
    setAttaching(true);
    setError("");
    try {
      const attachments: Attachment[] = [];
      // 図・レイアウトが必要な場合だけ、構造化テキストに原画像も添える。
      for (let i = 0; i < selections.length; i++) {
        const selection = selections[i];
        if (!doc || (!includeImages && selection.text.trim())) continue;
        const blob = await regionPng(doc, selection);
        if (!isCurrent()) return;
        const node = await uploadNewFile({
          file: new File(
            [blob],
            `${name}-v${version}-p${selection.page}-抜粋${i + 1}.png`,
            { type: "image/png" },
          ),
        });
        if (!isCurrent()) return;
        attachments.push({ node_id: node.id, name: node.name });
      }
      if (!isCurrent()) return;
      setPendingSelection(
        context,
        destination === "new" ? "home" : `pdf:${id}`,
      );
      queuePdfAttachments(
        attachments,
        destination === "new" ? "home" : `pdf:${id}`,
      );
      if (destination === "new") router.push("/");
      else {
        setAssistantOpen(true);
        setMode("read");
        setMobilePanel(false);
      }
    } catch (e) {
      if (isCurrent())
        setError(e instanceof Error ? e.message : "添付を準備できませんでした");
    } finally {
      if (isCurrent()) setAttaching(false);
    }
  };
  const retry = () => {
    if (!doc || !lastRegion.current) return;
    const { page, bbox, id: replaceId } = lastRegion.current;
    resetPdfBlocks(doc, page);
    resetPageText(doc, page);
    setTextRevision((n) => n + 1);
    // 失敗した領域の再試行。空のカードがあるときは重複させず差し替える。
    void onSelect(page, bbox, replaceId);
  };
  const panel =
    doc &&
    (mode === "tools" ? (
      <PdfUtilities doc={doc} name={name} current={current} />
    ) : (
      <PdfExcerptPanel
        doc={doc}
        selections={selections}
        busy={busy || attaching}
        error={error}
        includeImages={includeImages}
        onIncludeImages={setIncludeImages}
        onRemove={(key) => {
          attachmentGeneration.current++;
          setAttaching(false);
          replacements.current.delete(key);
          if (lastRegion.current?.id === key) {
            lastRegion.current = null;
            setError("");
          }
          setSelections((prev) => prev.filter((s) => s.id !== key));
        }}
        onClear={() => {
          invalidate();
          replacements.current.clear();
          lastRegion.current = null;
          pendingCount.current = 0;
          setBusy(false);
          setAttaching(false);
          setSelections([]);
          setError("");
        }}
        onJump={jumpTo}
        onAttach={attach}
        onRetry={retry}
      />
    ));
  const toolbar = (
    <div
      className="flex min-h-12 shrink-0 flex-wrap items-center justify-between gap-2 border-b border-border/60 bg-background px-3 py-2 md:px-4"
      data-testid="pdf-workspace-toolbar"
    >
      <div
        className="flex items-center gap-0.5 rounded-lg bg-muted/70 p-1"
        aria-label="PDF の操作モード"
      >
        {(
          [
            { key: "read", label: "閲覧", icon: BookOpen },
            { key: "select", label: "範囲選択", icon: MousePointer2 },
            { key: "tools", label: "ユーティリティ", icon: SlidersHorizontal },
          ] as const
        ).map(({ key, label, icon: Icon }) => (
          <button
            key={key}
            type="button"
            aria-pressed={mode === key}
            data-testid={`pdf-mode-${key}`}
            onClick={() => {
              setMode(key);
              if (key !== "read") setAssistantOpen(false);
              if (key === "tools") setMobilePanel(true);
            }}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              mode === key
                ? "bg-background font-medium text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <Icon className="size-3.5" />
            <span>{label}</span>
          </button>
        ))}
      </div>
      <div className="flex items-center gap-3">
        <button
          type="button"
          aria-label="文字情報を再読み込み"
          title="文字情報を再読み込み"
          onClick={() => {
            if (!doc) return;
            resetDocumentBlocks(doc);
            resetDocumentText(doc);
            setTextRevision((n) => n + 1);
          }}
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <RefreshCw className="size-3.5" />
        </button>
        {mode === "select" ? (
          <>
            <span className="hidden items-center gap-1.5 text-[11px] text-muted-foreground lg:flex">
              {busy ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <ScanText className="size-3.5" />
              )}
              ドラッグで追加 · Escで中止
            </span>
            <button
              type="button"
              onClick={() => setMobilePanel(true)}
              className="flex items-center gap-1 rounded-full bg-primary/10 px-2.5 py-1 text-[11px] font-medium text-primary md:hidden"
            >
              抜粋 {selections.length} 件<ChevronRight className="size-3" />
            </button>
          </>
        ) : (
          <button
            type="button"
            onClick={() => setAssistantOpen((o) => !o)}
            aria-pressed={assistantOpen}
            aria-label="アシスタント"
            className="flex items-center gap-1.5 rounded-md px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <MessageSquare className="size-3.5" />
            <span className="max-sm:hidden">アシスタント</span>
          </button>
        )}
      </div>
    </div>
  );
  return {
    toolbar,
    mode,
    selections,
    onSelect,
    textRevision,
    assistantOpen,
    closeAssistant: () => setAssistantOpen(false),
    sidePanel: mode !== "read" && (
      <>
        <aside
          className="hidden w-[320px] shrink-0 border-l border-border/60 bg-background md:block"
          aria-label={mode === "tools" ? "PDF ユーティリティ" : "選択した抜粋"}
        >
          {panel}
        </aside>
        {isMobile && (
          <Sheet open={mobilePanel} onOpenChange={setMobilePanel}>
            <SheetContent
              side="right"
              className="w-[360px] max-w-[95vw] p-0 pt-10"
              aria-describedby={undefined}
            >
              <SheetTitle className="sr-only">
                {mode === "tools" ? "PDF ユーティリティ" : "選択した抜粋"}
              </SheetTitle>
              {panel}
            </SheetContent>
          </Sheet>
        )}
      </>
    ),
    assistantPanel: scopeReady ? (
      <PdfAssistant key={`${id}:${authGeneration}`} id={id} name={name} />
    ) : null,
  };
}
