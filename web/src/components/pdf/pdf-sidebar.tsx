"use client";

/// PDF ビューアの横のパネル（ページのサムネイル一覧と目次）。
///
/// PC はビューアの左に並べ、スマホは左からのシートで開く（ビューア側が出し分ける）。
/// サムネイルは本体と同じく画面に近いものだけを描く（PdfPage の遅延描画）。
import * as React from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { PdfPage } from "@/components/pdf/pdf-page";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { cn } from "@/lib/utils";

/// サムネイルの幅（CSS px）。
const THUMB_WIDTH = 112;

type OutlineNode = { title: string; page: number | null; items: OutlineNode[] };

type RawOutline = { title: string; dest: string | unknown[] | null; items: RawOutline[] };

/// 目次の行き先（名前付き／明示の宛先）をページ番号へ解決する。解決できなければ null。
async function destPage(doc: PDFDocumentProxy, dest: string | unknown[] | null): Promise<number | null> {
  try {
    const explicit = typeof dest === "string" ? await doc.getDestination(dest) : dest;
    if (!Array.isArray(explicit) || explicit.length === 0) return null;
    const ref = explicit[0];
    if (typeof ref === "number") return ref + 1;
    if (ref && typeof ref === "object") {
      return (await doc.getPageIndex(ref as Parameters<PDFDocumentProxy["getPageIndex"]>[0])) + 1;
    }
  } catch {
    /* 壊れた宛先は飛べない項目として出す */
  }
  return null;
}

async function resolveOutline(doc: PDFDocumentProxy, items: RawOutline[]): Promise<OutlineNode[]> {
  return Promise.all(
    items.map(async (it) => ({
      title: it.title,
      page: await destPage(doc, it.dest),
      items: await resolveOutline(doc, it.items ?? []),
    })),
  );
}

/// 文書の目次（無ければ空配列・読み込み中は null）。
function useOutline(doc: PDFDocumentProxy): OutlineNode[] | null {
  const [outline, setOutline] = React.useState<OutlineNode[] | null>(null);
  React.useEffect(() => {
    let active = true;
    doc
      .getOutline()
      .then((raw) => resolveOutline(doc, (raw ?? []) as RawOutline[]))
      .then((o) => active && setOutline(o))
      .catch(() => active && setOutline([]));
    return () => {
      active = false;
    };
  }, [doc]);
  return outline;
}

export function PdfSidebar({
  doc,
  current,
  rotation,
  onJump,
  className,
}: {
  doc: PDFDocumentProxy;
  current: number;
  rotation: number;
  onJump: (page: number) => void;
  className?: string;
}) {
  const outline = useOutline(doc);
  const [tab, setTab] = React.useState<"pages" | "outline">("pages");
  const hasOutline = (outline?.length ?? 0) > 0;
  const listRef = React.useRef<HTMLDivElement>(null);

  // 今のページのサムネイルを見える位置に保つ（本体をスクロールしたとき）。
  React.useEffect(() => {
    if (tab !== "pages") return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-thumb="${current}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [current, tab]);

  return (
    <div className={cn("flex min-h-0 flex-col", className)} data-testid="pdf-sidebar">
      {hasOutline ? (
        <div className="flex shrink-0 justify-center px-3 pb-2 pt-3">
          <SegmentedControl
            aria-label="表示する一覧"
            size="sm"
            value={tab}
            onValueChange={(v) => setTab(v as "pages" | "outline")}
            options={[
              { value: "pages", label: "ページ", testId: "pdf-sidebar-pages" },
              { value: "outline", label: "目次", testId: "pdf-sidebar-outline" },
            ]}
          />
        </div>
      ) : null}
      <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 py-3 scrollbar-subtle">
        {tab === "outline" && hasOutline ? (
          <OutlineTree nodes={outline ?? []} current={current} onJump={onJump} depth={0} />
        ) : (
          <ol className="flex flex-col items-center gap-3">
            {Array.from({ length: doc.numPages }, (_, i) => i + 1).map((n) => (
              <li key={n}>
                <button
                  type="button"
                  data-thumb={n}
                  aria-label={`${n} ページへ移動`}
                  aria-current={n === current ? "page" : undefined}
                  onClick={() => onJump(n)}
                  className="group flex flex-col items-center gap-1 rounded-md p-1 outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <PdfPage
                    doc={doc}
                    pageNumber={n}
                    width={THUMB_WIDTH}
                    rotation={rotation}
                    className={cn(
                      "transition-shadow",
                      n === current ? "ring-2 ring-primary" : "group-hover:ring-foreground/25",
                    )}
                  />
                  <span
                    className={cn(
                      "text-[11px] tabular-nums",
                      n === current ? "font-semibold text-foreground" : "text-muted-foreground",
                    )}
                  >
                    {n}
                  </span>
                </button>
              </li>
            ))}
          </ol>
        )}
      </div>
    </div>
  );
}

function OutlineTree({
  nodes,
  current,
  onJump,
  depth,
}: {
  nodes: OutlineNode[];
  current: number;
  onJump: (page: number) => void;
  depth: number;
}) {
  return (
    <ul className="flex flex-col">
      {nodes.map((n, i) => (
        <li key={i}>
          <button
            type="button"
            disabled={n.page == null}
            onClick={() => n.page != null && onJump(n.page)}
            className={cn(
              "flex w-full items-baseline gap-2 rounded-md py-1.5 pr-2 text-left text-[13px] leading-snug outline-none",
              "hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 pointer-coarse:py-2.5",
              n.page === current ? "font-medium text-foreground" : "text-foreground/80",
            )}
            style={{ paddingLeft: 8 + depth * 14 }}
          >
            <span className="min-w-0 flex-1 break-words [overflow-wrap:anywhere]">{n.title}</span>
            {n.page != null ? (
              <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">{n.page}</span>
            ) : null}
          </button>
          {n.items.length > 0 ? (
            <OutlineTree nodes={n.items} current={current} onJump={onJump} depth={depth + 1} />
          ) : null}
        </li>
      ))}
    </ul>
  );
}
