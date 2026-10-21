"use client";

/// PDF ビューア（引用箇所へのディープリンク・#508）。
///
/// `/pdf/[id]?v=&page=&box=` で開くと、その版の該当ページへスクロールし、引用箇所の枠を
/// 重ねる（枠は Docling の prov。スキャン PDF でも OCR の座標で同じように出る）。
/// 版を省くと最新版を開く。ページは画面に近づいたものから描く（大きな PDF でも重くしない）。
///
/// 読む操作: 文字の選択・コピー（テキスト層）／文書内検索（⌘F）／ページ番号での移動／
/// サムネイル・目次のパネル／幅・ページ全体に合わせる／回転／拡大縮小（ボタン・⌘±・ピンチ）。
import * as React from "react";
import { useParams, useSearchParams } from "next/navigation";
import {
  Download,
  FileClock,
  FileWarning,
  Maximize,
  MoveHorizontal,
  PanelLeft,
  RotateCw,
  Search,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import type { PDFDocumentProxy } from "pdfjs-dist";

import { PageJump } from "@/components/pdf/page-jump";
import { PdfPage } from "@/components/pdf/pdf-page";
import { PdfSearchBar } from "@/components/pdf/pdf-search-bar";
import { PdfSidebar } from "@/components/pdf/pdf-sidebar";
import { usePdfSearch } from "@/components/pdf/use-pdf-search";
import { usePinchZoom } from "@/components/pdf/use-pinch-zoom";
import { CiteHint } from "@/components/shell/cite-hint";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
  useEditorBack,
} from "@/components/shell/editor-loading";
import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { useMediaQuery } from "@/hooks/use-media-query";
import { parseCiteBoxes } from "@/lib/citation-locate";
import { loadPdf } from "@/lib/pdf";
import { getNode, versionDownloadUrl, type NodeResponse } from "@/lib/storage";

type State =
  | { phase: "loading" }
  | { phase: "ready"; node: NodeResponse; version: number; doc: PDFDocumentProxy }
  | { phase: "error"; message: string };

/// 拡大縮小の段階（「幅に合わせる」に対する倍率）。幅を超えた分は横スクロールで見る。
const ZOOM_STEPS = [0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3, 4];
const ZOOM_MIN = ZOOM_STEPS[0];
const ZOOM_MAX = ZOOM_STEPS[ZOOM_STEPS.length - 1];
/// 幅フィットの上限（広い画面で 1 ページが巨大にならないよう、読みやすい幅で止める）。
const FIT_MAX = 880;
/// スクロール枠の内側の余白（上下・左右）。
const PAD = 16;
/// サイドパネルの開閉を覚えておくキー（PC のみ）。
const SIDEBAR_KEY = "shiki.pdf.sidebar";

/// 拡大縮小・回転の前後で見ている場所を保つための目印（焦点の下にあったページと、その中の位置）。
type Anchor = { page: number; fx: number; fy: number; rx: number; ry: number };

function isTypingTarget(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  return t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName);
}

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
  const [zoom, setZoom] = React.useState(1);
  const [rotation, setRotation] = React.useState(0);
  const [box, setBox] = React.useState({ w: 0, h: 0 });
  const [current, setCurrent] = React.useState(1);
  const scrollRef = React.useRef<HTMLDivElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  const jumped = React.useRef(false);
  // 1 ページ目の寸法（PDF ポイント・回転前）。まだ描いていないページの高さの見積もりと、倍率表示に使う。
  const [base, setBase] = React.useState({ w: 595, h: 842 });
  const isMobile = useMediaQuery("(max-width: 767px)");
  const [sidebarDesktop, setSidebarDesktop] = React.useState(false);
  const [sidebarMobile, setSidebarMobile] = React.useState(false);
  const sidebarOpen = isMobile ? sidebarMobile : sidebarDesktop;

  React.useEffect(() => {
    try {
      setSidebarDesktop(window.localStorage.getItem(SIDEBAR_KEY) === "1");
    } catch {
      /* 保存領域が使えない環境では閉じたまま */
    }
  }, []);
  const toggleSidebar = React.useCallback(() => {
    if (isMobile) {
      setSidebarMobile((o) => !o);
      return;
    }
    setSidebarDesktop((o) => {
      try {
        window.localStorage.setItem(SIDEBAR_KEY, o ? "0" : "1");
      } catch {
        /* 覚えられなくても開閉はできる */
      }
      return !o;
    });
  }, [isMobile]);

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
        setBase({ w: vp.width, h: vp.height });
        setState({ phase: "ready", node, version, doc });
      })
      .catch((e: unknown) => {
        if (active) setState({ phase: "error", message: e instanceof Error ? e.message : "読み込みに失敗しました" });
      });
    return () => {
      active = false;
    };
  }, [id, askedVersion]);

  const sideways = rotation % 180 !== 0;
  const aspect = sideways ? base.w / base.h : base.h / base.w;
  const fitWidth = Math.max(240, Math.min(FIT_MAX, box.w - PAD * 2));
  const pageWidth = Math.round(fitWidth * zoom);
  // 「ページ全体」: 1 ページが縦横とも枠に収まる倍率。
  const fitPageZoom = Math.min(1, Math.max(0.1, (box.h - PAD * 2) / aspect / fitWidth));
  // 等倍（96dpi で PDF の実寸）に対する割合。
  const percent = Math.round((pageWidth / ((sideways ? base.h : base.w) * (96 / 72))) * 100);

  // --- 見ている場所を保ったまま倍率・回転を変える --------------------------------
  const anchorRef = React.useRef<Anchor | null>(null);
  const captureAnchor = React.useCallback(
    (focal?: { x: number; y: number }): Anchor | null => {
      const root = scrollRef.current;
      if (!root) return null;
      const r = root.getBoundingClientRect();
      const fx = focal?.x ?? root.clientWidth / 2;
      const fy = focal?.y ?? root.clientHeight / 3;
      const hit = document
        .elementsFromPoint(r.left + fx, r.top + fy)
        .find((el): el is HTMLElement => el instanceof HTMLElement && el.dataset.page != null && root.contains(el));
      const page = hit ? Number(hit.dataset.page) : current;
      const el = hit ?? root.querySelector<HTMLElement>(`[data-page="${page}"]`);
      if (!el) return null;
      const pr = el.getBoundingClientRect();
      return {
        page,
        fx,
        fy,
        rx: pr.width > 0 ? (r.left + fx - pr.left) / pr.width : 0.5,
        ry: pr.height > 0 ? (r.top + fy - pr.top) / pr.height : 0,
      };
    },
    [current],
  );
  // 表示枠の大きさを追い、「幅に合わせる」の幅を決めて倍率を掛ける。幅が変わる（パネルの開閉・
  // 窓の大きさ）とページの幅も変わるので、見ていた場所を目印にして保つ。
  const captureRef = React.useRef(captureAnchor);
  captureRef.current = captureAnchor;
  React.useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const measure = () =>
      setBox((prev) => {
        if (prev.w === el.clientWidth && prev.h === el.clientHeight) return prev;
        if (prev.w !== el.clientWidth) anchorRef.current = captureRef.current();
        return { w: el.clientWidth, h: el.clientHeight };
      });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, [state.phase]);
  const zoomTo = React.useCallback(
    (next: number, focal?: { x: number; y: number }) => {
      const z = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, next));
      anchorRef.current = captureAnchor(focal);
      setZoom(z);
    },
    [captureAnchor],
  );
  const zoomStep = React.useCallback(
    (dir: 1 | -1) => {
      const next =
        dir > 0 ? ZOOM_STEPS.find((s) => s > zoom * 1.01) : [...ZOOM_STEPS].reverse().find((s) => s < zoom * 0.99);
      if (next != null) zoomTo(next);
    },
    [zoom, zoomTo],
  );
  const rotate = React.useCallback(() => {
    const a = captureAnchor();
    // 回転すると縦横が入れ替わるので、ページの中の位置ではなくページの先頭に合わせる。
    anchorRef.current = a ? { ...a, rx: 0.5, ry: 0, fy: PAD } : null;
    setRotation((r) => (r + 90) % 360);
  }, [captureAnchor]);
  React.useLayoutEffect(() => {
    const a = anchorRef.current;
    const root = scrollRef.current;
    if (!a || !root) return;
    anchorRef.current = null;
    const el = root.querySelector<HTMLElement>(`[data-page="${a.page}"]`);
    if (!el) return;
    const r = root.getBoundingClientRect();
    const pr = el.getBoundingClientRect();
    root.scrollTop += pr.top - r.top + a.ry * pr.height - a.fy;
    root.scrollLeft += pr.left - r.left + a.rx * pr.width - a.fx;
  }, [pageWidth, rotation]);

  usePinchZoom({
    enabled: state.phase === "ready",
    rootRef: scrollRef,
    contentRef,
    clamp: (ratio) => Math.min(ZOOM_MAX / zoom, Math.max(ZOOM_MIN / zoom, ratio)),
    onCommit: (ratio, focal) => zoomTo(zoom * ratio, focal),
  });

  // --- ページ移動 ----------------------------------------------------------------
  const numPages = state.phase === "ready" ? state.doc.numPages : 0;
  const jumpTo = React.useCallback(
    (page: number) => {
      const root = scrollRef.current;
      const el = root?.querySelector<HTMLElement>(`[data-page="${page}"]`);
      if (!root || !el) return;
      root.scrollTop += el.getBoundingClientRect().top - root.getBoundingClientRect().top - PAD;
      setCurrent(page);
    },
    [],
  );

  // 引用のページへ移る（ページの寸法が決まってから 1 回だけ）。
  React.useEffect(() => {
    if (state.phase !== "ready" || jumped.current || !targetPage || box.w === 0) return;
    const t = window.setTimeout(() => {
      const root = scrollRef.current;
      const page = root?.querySelector<HTMLElement>(`[data-page="${targetPage}"]`);
      if (!root || !page) return;
      jumped.current = true;
      const citeBox = page.querySelector<HTMLElement>("[data-cite-box]");
      const anchor = citeBox ?? page;
      const top = anchor.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop;
      root.scrollTo({ top: Math.max(0, top - (citeBox ? root.clientHeight / 3 : PAD)) });
    }, 120);
    return () => window.clearTimeout(t);
  }, [state, targetPage, box.w, pageWidth]);

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
        .find((el): el is HTMLElement => el instanceof HTMLElement && el.dataset.page != null && root.contains(el));
      if (hit) setCurrent(Number(hit.dataset.page));
    });
  }, []);

  // --- 文書内検索 ----------------------------------------------------------------
  const search = usePdfSearch(state.phase === "ready" ? state.doc : null, current);
  const searchInputRef = React.useRef<HTMLInputElement>(null);
  const openSearch = React.useCallback(() => {
    search.setOpen(true);
    window.requestAnimationFrame(() => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    });
  }, [search]);
  const closeSearch = React.useCallback(() => {
    search.setOpen(false);
    scrollRef.current?.focus({ preventScroll: true });
  }, [search]);
  // 今の一致のページが画面外なら、まずそのページへ移る（描かれたらページが一致の位置まで寄せる）。
  const activePage = search.active?.page ?? null;
  React.useEffect(() => {
    if (activePage == null) return;
    const root = scrollRef.current;
    const el = root?.querySelector<HTMLElement>(`[data-page="${activePage}"]`);
    if (!root || !el) return;
    const r = root.getBoundingClientRect();
    const pr = el.getBoundingClientRect();
    if (pr.bottom < r.top || pr.top > r.bottom) jumpTo(activePage);
  }, [activePage, search.index, jumpTo]);

  // --- キーボード ----------------------------------------------------------------
  React.useEffect(() => {
    if (state.phase !== "ready") return;
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        openSearch();
        return;
      }
      if ((mod && e.key.toLowerCase() === "g") || e.key === "F3") {
        if (!search.open) return;
        e.preventDefault();
        if (e.shiftKey) search.prev();
        else search.next();
        return;
      }
      if (mod && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        zoomStep(1);
        return;
      }
      if (mod && e.key === "-") {
        e.preventDefault();
        zoomStep(-1);
        return;
      }
      if (mod && e.key === "0") {
        e.preventDefault();
        zoomTo(1);
        return;
      }
      if (e.key === "Escape" && search.open) {
        closeSearch();
        return;
      }
      if (mod || e.altKey || isTypingTarget(e.target)) return;
      const root = scrollRef.current;
      // 横にはみ出していないときだけ ←/→ をページ送りにする（はみ出していれば横スクロールに使う）。
      const wide = root ? root.scrollWidth > root.clientWidth + 1 : false;
      if (e.key === "ArrowRight" && !wide) {
        e.preventDefault();
        jumpTo(Math.min(numPages, current + 1));
      } else if (e.key === "ArrowLeft" && !wide) {
        e.preventDefault();
        jumpTo(Math.max(1, current - 1));
      } else if (e.key === "Home") {
        e.preventDefault();
        jumpTo(1);
      } else if (e.key === "End") {
        e.preventDefault();
        jumpTo(numPages);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [state.phase, search, openSearch, closeSearch, zoomStep, zoomTo, jumpTo, numPages, current]);

  // 開いたら本体にフォーカスを置く（↑↓・PageUp/Down でそのままスクロールできるように）。
  React.useEffect(() => {
    if (state.phase === "ready") scrollRef.current?.focus({ preventScroll: true });
  }, [state.phase]);

  const version = state.phase === "ready" ? state.version : null;
  const download = React.useCallback(async () => {
    if (version == null) return;
    const t = await versionDownloadUrl(id, version);
    window.location.href = t.url;
  }, [id, version]);

  // クローム（戻る → パネル・検索・縮小・拡大 → ⋯（表示・回転・ダウンロード・最新版））。
  // 読み込み中・エラーでも戻る。
  const back = useEditorBack();
  const name = state.phase === "ready" ? state.node.name.replace(/\.pdf$/i, "") : "";
  const latest = state.phase === "ready" ? state.node.version : null;
  const stale = version != null && latest != null && version < latest;
  usePageChrome(() => {
    if (version == null) return { back };
    return {
      title: name,
      menuTitle: name,
      back,
      subtitle: (
        <span className="inline-flex items-center gap-2">
          <PageJump current={current} total={numPages} onJump={jumpTo} suffix="ページ" />
          <span className="tabular-nums" data-testid="pdf-zoom-percent">
            {percent}%
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
            label="ページ一覧・目次"
            pressed={sidebarOpen}
            onClick={toggleSidebar}
            data-testid="pdf-sidebar-toggle"
          >
            <PanelLeft aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton
            label="文書内を検索（⌘F）"
            pressed={search.open}
            onClick={() => (search.open ? closeSearch() : openSearch())}
            data-testid="pdf-search-toggle"
          >
            <Search aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton
            label="縮小（⌘−）"
            disabled={zoom <= ZOOM_MIN}
            onClick={() => zoomStep(-1)}
            className="disabled:pointer-events-none disabled:opacity-35 max-md:hidden"
          >
            <ZoomOut aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton
            label="拡大（⌘＋）"
            disabled={zoom >= ZOOM_MAX}
            onClick={() => zoomStep(1)}
            className="disabled:pointer-events-none disabled:opacity-35 max-md:hidden"
          >
            <ZoomIn aria-hidden />
          </ChromeIconButton>
        </>
      ),
      menu: [
        { key: "fit-width", label: "幅に合わせる", icon: MoveHorizontal, onSelect: () => zoomTo(1), testId: "pdf-fit-width" },
        {
          key: "fit-page",
          label: "ページ全体を表示",
          icon: Maximize,
          onSelect: () => zoomTo(fitPageZoom),
          testId: "pdf-fit-page",
        },
        { key: "rotate", label: "右に回転", icon: RotateCw, onSelect: rotate, testId: "pdf-rotate" },
        {
          key: "download",
          label: "ダウンロード",
          icon: Download,
          onSelect: () => void download(),
          testId: "pdf-menu-download",
          separatorBefore: true,
        },
        ...(stale
          ? [{ key: "latest", label: `最新版（v${latest}）を開く`, icon: FileClock, href: `/pdf/${id}` }]
          : []),
      ],
    };
  }, [
    back,
    version,
    name,
    current,
    numPages,
    stale,
    latest,
    zoom,
    percent,
    fitPageZoom,
    download,
    id,
    sidebarOpen,
    toggleSidebar,
    search.open,
    openSearch,
    closeSearch,
    zoomStep,
    zoomTo,
    rotate,
    jumpTo,
  ]);

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
  const onSidebarJump = (page: number) => {
    jumpTo(page);
    if (isMobile) setSidebarMobile(false);
  };
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
      <div className="flex min-h-0 flex-1">
        {sidebarOpen && !isMobile ? (
          <PdfSidebar
            doc={doc}
            current={current}
            rotation={rotation}
            onJump={onSidebarJump}
            className="w-[168px] shrink-0 border-r border-border/60 bg-background"
          />
        ) : null}
        <div className="relative min-h-0 min-w-0 flex-1">
          <div
            ref={scrollRef}
            onScroll={onScroll}
            tabIndex={0}
            aria-label={`${name}（${numPages} ページ）`}
            className="h-full touch-pan-x touch-pan-y overflow-auto overscroll-contain bg-muted/40 py-4 outline-none focus-visible:ring-0 focus-visible:ring-offset-0"
            data-testid="pdf-viewer"
          >
            {/* w-max: 拡大して表示幅を超えたら横スクロール（中央寄せで左端が切れないように）。 */}
            <div ref={contentRef} className="flex w-max min-w-full flex-col items-center gap-4 px-4">
              {box.w > 0
                ? Array.from({ length: doc.numPages }, (_, i) => i + 1).map((n) => (
                    <PdfPage
                      key={n}
                      doc={doc}
                      pageNumber={n}
                      width={pageWidth}
                      estimate={pageWidth * aspect}
                      boxes={boxes}
                      rotation={rotation}
                      textLayer
                      highlights={search.byPage.get(n)}
                      eager={n === targetPage || n === activePage}
                    />
                  ))
                : null}
            </div>
          </div>
          {search.open ? (
            <div className="absolute inset-x-3 top-3 z-20 flex justify-end md:left-auto md:w-[380px]">
              <PdfSearchBar
                inputRef={searchInputRef}
                query={search.query}
                onQueryChange={search.setQuery}
                total={search.matches.length}
                index={search.index}
                scanning={search.scanning}
                onNext={search.next}
                onPrev={search.prev}
                onClose={closeSearch}
                className="w-full"
              />
            </div>
          ) : null}
          {/* スマホ: 今のページ（PC はバーに出す）。番号を打って移動できる。 */}
          {!search.open ? (
            <div className="absolute right-3 top-3 z-10 rounded-full bg-background/90 px-2 py-1 text-[12px] font-medium text-muted-foreground shadow-sm ring-1 ring-border/60 md:hidden">
              <PageJump current={current} total={numPages} onJump={jumpTo} />
            </div>
          ) : null}
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
      {isMobile ? (
        <Sheet open={sidebarMobile} onOpenChange={setSidebarMobile}>
          <SheetContent side="left" className="max-w-[220px] pt-12" aria-describedby={undefined}>
            <SheetTitle className="sr-only">ページ一覧・目次</SheetTitle>
            <PdfSidebar doc={doc} current={current} rotation={rotation} onJump={onSidebarJump} className="h-full" />
          </SheetContent>
        </Sheet>
      ) : null}
    </div>
  );
}
