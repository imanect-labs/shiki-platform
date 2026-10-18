"use client";

/// PDF ビューア（引用箇所へのディープリンク・#508）。
///
/// `/pdf/[id]?v=&page=&box=` で開くと、その版の該当ページへスクロールし、引用箇所の枠を
/// 重ねる（枠は Docling の prov。スキャン PDF でも OCR の座標で同じように出る）。
/// 版を省くと最新版を開く。ページは画面に近づいたものから描く（大きな PDF でも重くしない）。
import * as React from "react";
import { useParams, useSearchParams } from "next/navigation";
import { Download, FileClock, FileWarning, ZoomIn, ZoomOut } from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { PdfPage } from "@/components/pdf/pdf-page";
import { CiteHint } from "@/components/shell/cite-hint";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
  useEditorBack,
} from "@/components/shell/editor-loading";
import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { parseCiteBoxes } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { getNode, versionDownloadUrl, type NodeResponse } from "@/lib/storage";

type State =
  | { phase: "loading" }
  | { phase: "ready"; node: NodeResponse; version: number; doc: PDFDocumentProxy }
  | { phase: "error"; message: string };

/// ズームの段階（「幅フィット」に対する倍率）。幅を超えた分は横スクロールで見る。
const ZOOMS = [1, 1.5, 2, 3];
/// 幅フィットの上限（広い画面で 1 ページが巨大にならないよう、読みやすい幅で止める）。
const FIT_MAX = 880;

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
  const [zoom, setZoom] = React.useState(0);
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

  // 表示幅に合わせて「幅フィット」を決め、倍率を掛けてページ幅にする。
  React.useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setContainerWidth(el.clientWidth));
    ro.observe(el);
    setContainerWidth(el.clientWidth);
    return () => ro.disconnect();
  }, [state.phase]);
  const fitWidth = Math.max(240, Math.min(FIT_MAX, containerWidth - 32));
  const pageWidth = Math.round(fitWidth * ZOOMS[zoom]);

  // 拡大・縮小しても今見ているページに留まる（ページ幅が変わると縦位置がずれるため）。
  const currentRef = React.useRef(current);
  currentRef.current = current;
  const lastZoom = React.useRef(zoom);
  React.useEffect(() => {
    if (lastZoom.current === zoom) return;
    lastZoom.current = zoom;
    const id = window.requestAnimationFrame(() => {
      const root = scrollRef.current;
      const page = root?.querySelector<HTMLElement>(`[data-page="${currentRef.current}"]`);
      if (!root || !page) return;
      root.scrollTop =
        page.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop - 16;
    });
    return () => window.cancelAnimationFrame(id);
  }, [zoom]);

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

  const version = state.phase === "ready" ? state.version : null;
  const download = React.useCallback(async () => {
    if (version == null) return;
    const t = await versionDownloadUrl(id, version);
    window.location.href = t.url;
  }, [id, version]);

  // クローム（戻る → 縮小・拡大 → ⋯（ダウンロード・最新版を開く））。読み込み中・エラーでも戻る。
  const back = useEditorBack();
  const name = state.phase === "ready" ? state.node.name.replace(/\.pdf$/i, "") : "";
  const latest = state.phase === "ready" ? state.node.version : null;
  const stale = version != null && latest != null && version < latest;
  const numPages = state.phase === "ready" ? state.doc.numPages : 0;
  usePageChrome(() => {
    if (version == null) return { back };
    return {
      title: name,
      menuTitle: name,
      back,
      subtitle: (
        <span className="inline-flex items-center gap-2">
          <span className="tabular-nums" aria-live="polite">
            {current} / {numPages} ページ
          </span>
          {stale ? (
            <EditorStatusBadge icon={FileClock}>
              v{version}（最新は v{latest}）
            </EditorStatusBadge>
          ) : null}
        </span>
      ),
      actions: (
        <>
          <ChromeIconButton
            label="縮小"
            disabled={zoom === 0}
            onClick={() => setZoom((z) => Math.max(0, z - 1))}
            className="disabled:pointer-events-none disabled:opacity-35"
          >
            <ZoomOut aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton
            label="拡大"
            disabled={zoom === ZOOMS.length - 1}
            onClick={() => setZoom((z) => Math.min(ZOOMS.length - 1, z + 1))}
            className="disabled:pointer-events-none disabled:opacity-35"
          >
            <ZoomIn aria-hidden />
          </ChromeIconButton>
        </>
      ),
      menu: [
        {
          key: "download",
          label: "ダウンロード",
          icon: Download,
          onSelect: () => void download(),
          testId: "pdf-menu-download",
        },
        ...(stale
          ? [{ key: "latest", label: `最新版（v${latest}）を開く`, icon: FileClock, href: `/pdf/${id}` }]
          : []),
      ],
    };
  }, [back, version, name, current, numPages, stale, latest, zoom, download, id]);

  if (state.phase === "loading") return <EditorLoading kind="doc" message="PDF を開いています…" />;
  if (state.phase === "error") {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="この PDF は開けません"
        description="ファイルが存在しないか、開く権限がないか、読み込みに失敗しました。"
      />
    );
  }

  const { doc } = state;
  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* スマホ: 文書名＋状態（PC はバーに出す）。 */}
      <EditorMobileHeading
        name={name}
        className="pb-1"
        status={
          stale ? (
            <EditorStatusBadge icon={FileClock}>
              v{version}（最新は v{latest}）
            </EditorStatusBadge>
          ) : null
        }
      />
      <div className="relative min-h-0 flex-1">
        <div
          ref={scrollRef}
          onScroll={onScroll}
          className="h-full overflow-auto overscroll-contain bg-muted/40 py-4"
          data-testid="pdf-viewer"
        >
          {/* w-max: 拡大して表示幅を超えたら横スクロール（中央寄せで左端が切れないように）。 */}
          <div className="flex w-max min-w-full flex-col items-center gap-4 px-4">
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
        {/* スマホ: 今のページ（PC はバーに出す）。 */}
        <div className="pointer-events-none absolute right-3 top-3 z-10 rounded-full bg-background/90 px-2.5 py-1 text-[12px] font-medium tabular-nums text-muted-foreground shadow-sm ring-1 ring-border/60 md:hidden">
          {current} / {doc.numPages}
        </div>
        {targetPage ? (
          <div className="pointer-events-none absolute inset-x-0 bottom-[max(1rem,env(safe-area-inset-bottom))] z-20 flex justify-center px-4">
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
