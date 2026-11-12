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
import { AVOID_ASSISTANT } from "@/components/shell/assistant-dock";
import {
  queuePdfAttachments,
  clearPdfAttachmentsForTarget,
} from "@/lib/pdf-attachment-handoff";
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
  PDF_LOCATOR_LIMIT,
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

import type { SelectionPrecision, FineContent } from "@/lib/pdf-fine-selection";

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
  const [precision, setPrecision] = React.useState<SelectionPrecision>("area");
  const [mode, setMode] = React.useState<Mode>("read");
  const [selections, setSelections] = React.useState<PdfExcerpt[]>([]);
  const selectionsRef = React.useRef(selections);
  React.useLayoutEffect(() => {
    selectionsRef.current = selections;
  }, [selections]);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState("");
  const [assistantOpen, setAssistantOpen] = React.useState(false);
  const [mobilePanel, setMobilePanel] = React.useState(false);
  const [includeImages, setIncludeImages] = React.useState(false);
  const [attaching, setAttaching] = React.useState(false);
  const [textRevision, setTextRevision] = React.useState(0);
  const generation = React.useRef(0);
  const attachmentGeneration = React.useRef(0);
  const pendingOperations = React.useRef(new Map<symbol, string | undefined>());
  const replacements = React.useRef(new Map<string, symbol>());
  const invalidate = React.useCallback(() => {
    generation.current++;
    attachmentGeneration.current++;
  }, []);
  const lastRegion = React.useRef<{
    page: number;
    bbox: PdfRect;
    id?: string;
    fine?: FineContent;
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
    pendingOperations.current.clear();
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
    async (
      page: number,
      bbox: PdfRect,
      replaceId?: string,
      fine?: FineContent,
    ) => {
      if (!doc) return;
      if (
        !replaceId &&
        selections.length +
          [...pendingOperations.current.values()].filter(
            (id) => id === undefined,
          ).length >=
          20
      ) {
        setError("一度に選べる範囲は20件までです。");
        return;
      }
      attachmentGeneration.current++;
      setAttaching(false);
      const replacementToken = Symbol();
      if (replaceId) {
        const previous = replacements.current.get(replaceId);
        if (previous) pendingOperations.current.delete(previous);
        replacements.current.set(replaceId, replacementToken);
      }
      if (replaceId && !fine)
        setSelections((prev) =>
          prev.map((s) =>
            s.id === replaceId ? { ...s, bbox, text: "", tables: [] } : s,
          ),
        );
      const request = { page, bbox, id: replaceId, fine };
      lastRegion.current = request;
      const at = generation.current;
      const scope = storageScopeGeneration();
      pendingOperations.current.set(replacementToken, replaceId);
      setBusy(true);
      setError("");
      try {
        const extracted = await extractRegion(doc, page, bbox);
        const result = { ...extracted, ...fine, id: replaceId ?? extracted.id };
        if (
          at !== generation.current ||
          scope !== storageScopeGeneration() ||
          (replaceId &&
            replacements.current.get(replaceId) !== replacementToken)
        )
          return;
        if (fine) {
          const next = [
            ...selectionsRef.current.filter((s) => s.id !== replaceId),
            result,
          ];
          if (
            JSON.stringify(excerptContext(id, name, version ?? 0, next).locator)
              .length > PDF_LOCATOR_LIMIT
          )
            throw new Error(
              "選択範囲が大きすぎます。範囲を小さくしてください。",
            );
        }
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
          lastRegion.current = { page, bbox, id: result.id, fine };
        if (!result.text) {
          lastRegion.current = { page, bbox, id: result.id, fine };
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
          pendingOperations.current.delete(replacementToken);
          setBusy(pendingOperations.current.size > 0);
        }
      }
    },
    [doc, selections.length, id, name, version],
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
      JSON.stringify(context.locator).length > PDF_LOCATOR_LIMIT
    ) {
      setError("添付上限を超えています。範囲を減らしてください。");
      return;
    }
    const at = generation.current;
    const scope = storageScopeGeneration();
    const attachmentAt = ++attachmentGeneration.current;
    const isCurrent = () =>
      at === generation.current &&
      scope === storageScopeGeneration() &&
      attachmentAt === attachmentGeneration.current;
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
    const { page, bbox, id: replaceId, fine } = lastRegion.current;
    resetPdfBlocks(doc, page);
    resetPageText(doc, page);
    setTextRevision((n) => n + 1);
    // 失敗した領域の再試行。空のカードがあるときは重複させず差し替える。
    void onSelect(page, bbox, replaceId, fine);
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
          const token = replacements.current.get(key);
          if (token) pendingOperations.current.delete(token);
          replacements.current.delete(key);
          setBusy(pendingOperations.current.size > 0);
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
          pendingOperations.current.clear();
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
      className={cn(
        "flex min-h-12 shrink-0 flex-wrap items-center justify-between gap-2 border-b border-border/60 bg-background px-3 py-2 md:px-4",
        // アシスタントパネルは上まで貫通するので、その下に潜らないよう右を空ける。
        AVOID_ASSISTANT,
      )}
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
      {mode === "select" && (
        <div
          role="group"
          aria-label="選択の粒度"
          className="flex rounded-lg border p-0.5 text-[11px]"
        >
          {(
            [
              ["area", "自由範囲"],
              ["line", "行"],
              ["cell", "表セル"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={value}
              type="button"
              aria-pressed={precision === value}
              onClick={() => setPrecision(value)}
              className={cn(
                "rounded-md px-3 py-1.5",
                precision === value
                  ? "bg-primary text-primary-foreground"
                  : "text-muted-foreground hover:bg-accent",
              )}
            >
              {label}
            </button>
          ))}
        </div>
      )}
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
              枠をつかんで調整 · Escで中止
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
    precision,
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
