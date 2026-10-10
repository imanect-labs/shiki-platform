"use client";

/// PDF ビューア（引用箇所へのディープリンク・#508）。
///
/// `/pdf/[id]?v=&page=&box=` で開くと、その版の該当ページへスクロールし、引用箇所の枠を
/// 重ねる（枠は Docling の prov。スキャン PDF でも OCR の座標で同じように出る）。
/// 版を省くと最新版を開く。ページは画面に近づいたものから描く（大きな PDF でも重くしない）。
import * as React from "react";
import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import { Download, FileWarning, Minus, Plus } from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { PdfPage } from "@/components/pdf/pdf-page";
import { CiteHint } from "@/components/shell/cite-hint";
import { EditorLoading } from "@/components/shell/editor-loading";
import { EmptyState } from "@/components/ui/empty-state";
import { parseCiteBoxes } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { getNode, versionDownloadUrl, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

type State =
  | { phase: "loading" }
  | { phase: "ready"; node: NodeResponse; version: number; doc: PDFDocumentProxy }
  | { phase: "error"; message: string };

/// ページ幅の段階（CSS px の上限。実際は表示領域に収まる幅まで縮める）。
const ZOOMS = [560, 720, 880, 1080, 1320];

export default function PdfPageRoute() {
  return (
    <React.Suspense fallback={<EditorLoading kind="doc" message="PDF を開いています…" />}>
      <PdfViewer />
    </React.Suspense>
  );
}

function PdfViewer() {
  const { id } = useParams<{ id: string }>();
  const sp = useSearchParams();
  const askedVersion = Number(sp.get("v")) || null;
  const targetPage = Number(sp.get("page")) || null;
  const boxes = React.useMemo(() => parseCiteBoxes(sp.getAll("box")), [sp]);
  const [state, setState] = React.useState<State>({ phase: "loading" });
  const [zoom, setZoom] = React.useState(2);
  const [containerWidth, setContainerWidth] = React.useState(0);
  const [current, setCurrent] = React.useState(1);
  const scrollRef = React.useRef<HTMLDivElement>(null);
  const jumped = React.useRef(false);
  // 1 ページ目の縦横比（まだ描いていないページの高さの見積もりに使う）。
  const [aspect, setAspect] = React.useState(1.414);

  React.useEffect(() => {
    let active = true;
    setState({ phase: "loading" });
    getNode(id)
      .then(async (node) => {
        const version = askedVersion ?? node.version;
        const doc = await loadPdf(id, version);
        const first = await doc.getPage(1);
        const vp = first.getViewport({ scale: 1 });
        if (!active) return;
        setAspect(vp.height / vp.width);
        setState({ phase: "ready", node, version, doc });
      })
      .catch((e: unknown) => {
        if (active) setState({ phase: "error", message: e instanceof Error ? e.message : "読み込みに失敗しました" });
      });
    return () => {
      active = false;
    };
  }, [id, askedVersion]);

  // 表示幅に合わせてページ幅を決める（狭い画面では収まる幅まで縮める）。
  React.useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setContainerWidth(el.clientWidth));
    ro.observe(el);
    setContainerWidth(el.clientWidth);
    return () => ro.disconnect();
  }, [state.phase]);
  const pageWidth = Math.max(240, Math.min(ZOOMS[zoom], containerWidth - 32));

  // 引用のページへ移る（ページの寸法が決まってから 1 回だけ）。
  React.useEffect(() => {
    if (state.phase !== "ready" || jumped.current || !targetPage || containerWidth === 0) return;
    const t = window.setTimeout(() => {
      const root = scrollRef.current;
      const page = root?.querySelector<HTMLElement>(`[data-page="${targetPage}"]`);
      if (!root || !page) return;
      jumped.current = true;
      const box = page.querySelector<HTMLElement>("[data-cite-box]");
      const anchor = box ?? page;
      const top = anchor.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop;
      root.scrollTo({ top: Math.max(0, top - (box ? root.clientHeight / 3 : 16)) });
    }, 120);
    return () => window.clearTimeout(t);
  }, [state, targetPage, containerWidth, pageWidth]);

  // 今見ているページ番号（ヘッダ表示）。スクロールごとに全ページを測らないよう、
  // フレームに 1 回だけ、上 1/3 の位置にある要素を引く。
  const frame = React.useRef<number | null>(null);
  const onScroll = React.useCallback(() => {
    if (frame.current != null) return;
    frame.current = window.requestAnimationFrame(() => {
      frame.current = null;
      const root = scrollRef.current;
      if (!root) return;
      const r = root.getBoundingClientRect();
      const hit = document
        .elementsFromPoint(r.left + r.width / 2, r.top + root.clientHeight / 3)
        .find((el): el is HTMLElement => el instanceof HTMLElement && el.dataset.page != null);
      if (hit) setCurrent(Number(hit.dataset.page));
    });
  }, []);

  const download = async () => {
    if (state.phase !== "ready") return;
    const t = await versionDownloadUrl(id, state.version);
    window.location.href = t.url;
  };

  if (state.phase === "loading") return <EditorLoading kind="doc" message="PDF を開いています…" />;
  if (state.phase === "error") {
    return (
      <EmptyState
        icon={FileWarning}
        title="この PDF は開けません"
        description="ファイルが存在しないか、開く権限がないか、読み込みに失敗しました。"
      />
    );
  }

  const { node, version, doc } = state;
  const stale = version < node.version;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-11 shrink-0 items-center gap-2 px-3 shiki-dash-bottom">
        <span className="min-w-0 truncate text-sm font-medium">{node.name.replace(/\.pdf$/i, "")}</span>
        {stale ? (
          <span className="shrink-0 rounded-md bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
            v{version}（最新は v{node.version}）
          </span>
        ) : null}
        <span className="ml-auto shrink-0 text-[12px] tabular-nums text-muted-foreground" aria-live="polite">
          {current} / {doc.numPages}
        </span>
        <div className="flex shrink-0 items-center">
          <IconButton label="縮小" disabled={zoom === 0} onClick={() => setZoom((z) => Math.max(0, z - 1))}>
            <Minus className="size-4" />
          </IconButton>
          <IconButton
            label="拡大"
            disabled={zoom === ZOOMS.length - 1}
            onClick={() => setZoom((z) => Math.min(ZOOMS.length - 1, z + 1))}
          >
            <Plus className="size-4" />
          </IconButton>
          <IconButton label="ダウンロード" onClick={() => void download()}>
            <Download className="size-4" />
          </IconButton>
        </div>
        {stale ? (
          <Link
            href={`/pdf/${id}`}
            className="shrink-0 rounded-lg px-2 py-1 text-[12px] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          >
            最新版を開く
          </Link>
        ) : null}
      </div>
      <div className="relative min-h-0 flex-1">
        <div
          ref={scrollRef}
          onScroll={onScroll}
          className="h-full overflow-auto bg-muted/40 py-4"
          data-testid="pdf-viewer"
        >
          <div className="flex flex-col items-center gap-4">
            {containerWidth > 0
              ? Array.from({ length: doc.numPages }, (_, i) => i + 1).map((n) => (
                  <PdfPage
                    key={`${n}:${pageWidth}`}
                    doc={doc}
                    pageNumber={n}
                    width={pageWidth}
                    estimate={pageWidth * aspect}
                    boxes={boxes}
                    eager={n === targetPage}
                  />
                ))
              : null}
          </div>
        </div>
        {targetPage ? (
          <div className="pointer-events-none absolute inset-x-0 bottom-4 z-20 flex justify-center">
            <CiteHint tone={boxes.length > 0 ? "found" : "approx"}>
              {boxes.length > 0
                ? `引用箇所（p.${targetPage}）を枠で示しています`
                : `引用のページ（p.${targetPage}）を開きました`}
            </CiteHint>
          </div>
        ) : null}
      </div>
    </div>
  );
}

function IconButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className={cn(
        "flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-35",
      )}
    >
      {children}
    </button>
  );
}
