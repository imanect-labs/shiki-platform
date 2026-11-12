"use client";

/// 文書エディタ（ノート・スライド・CSV・Office）の AI アシスタントの共通コンテナ。
///
/// - lg 以上: 画面の右端に縦いっぱい（上部バーの高さまで貫通）に浮かぶ角丸カード。本文は
///   パネル幅ぶんの右余白で、上部バーは `AVOID_ASSISTANT` でパネルを避ける（⋯ がパネルの左隣に
///   来る）。開閉と幅は `assistant-dock` 経由でシェルの列へ渡す。
///   左端のつまみをドラッグ（または ←/→ キー）で幅を変えられ、幅は画面の好みとして保存する
///   （ダブルクリックで既定幅へ戻す）。
/// - lg 未満: 下からの**非モーダル**なシート。初期は約 55dvh（半分）で、本文はシートの上に
///   見えたまま選択・編集できる（本文側にシートの高さぶんの下余白を入れる）。つまみのドラッグ
///   または ⤢ ボタンで「半分 ⇄ 全画面」、半分から大きく下へ引くと閉じる。
///
/// 開くたびに半分から始める（`?thread=` 付きで開いたとき・下書き保存直後も全画面にしない）。
/// パネルの中身（会話）は 1 つだけマウントし、幅に応じてクラスで形を変える（会話を二重に
/// 張らない）。

import { Maximize2, Minimize2, Sparkles, X } from "lucide-react";
import * as React from "react";

import { useAssistantDock } from "@/components/shell/assistant-dock";
import { useLocalStorage } from "@/hooks/use-local-storage";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";

type SheetMode = "half" | "full";

/// 半分のときのシートの高さ（本文の下余白と同じ値）。
const HALF_HEIGHT = "min(55dvh, 100%)";

/// PC のパネル幅（px）。既定・最小・最大と、キー操作 1 回の増減。
const DEFAULT_WIDTH = 420;
const MIN_WIDTH = 360;
const MAX_WIDTH = 960;
const KEY_STEP = 16;
/// 本文側に最低限残す幅（px）。パネルを広げても本文が潰れないようにする。
const MIN_BODY = 352;
/// パネルの右・左の余白（right-3 と本文との間）。本文の右余白 = 幅 + これ。
const GUTTER = 28;
const WIDTH_KEY = "shiki.assistant-panel-width";

/// 画面に入る最大幅。本文に MIN_BODY を残す（CSS の `calc(100%-380px)` と同じ式）。
/// 画面が狭いときは MIN_WIDTH を下回る（そのときは幅を変えられない）。
function maxWidthFor(rootWidth: number): number {
  return Math.max(0, Math.min(MAX_WIDTH, rootWidth - MIN_BODY - GUTTER));
}

function clampWidth(w: number, rootWidth: number): number {
  const max = maxWidthFor(rootWidth);
  return Math.round(Math.min(max, Math.max(MIN_WIDTH, w)));
}

export function AssistantLayout({
  open,
  onClose,
  label,
  title = "アシスタント",
  panel,
  keepMounted = false,
  overlay,
  className,
  children,
}: {
  open: boolean;
  onClose: () => void;
  /// complementary ランドマークの名前（例: 「ノートのアシスタント」）。
  label: string;
  /// パネル見出し。
  title?: string;
  /// パネルの中身（会話）。既定では open のときだけ描く。
  panel: React.ReactNode;
  /// 初回表示後は非表示でも保持し、未送信の本文・添付を失わない。
  keepMounted?: boolean;
  /// 本文の下端に重ねる案内（引用ハイライトのヒント等）。シートが開いていればその上に出す。
  overlay?: React.ReactNode;
  className?: string;
  /// 本文。`min-h-0 flex-1` の要素を置く（スクロールは本文側で持つ）。
  children: React.ReactNode;
}) {
  const rootRef = React.useRef<HTMLDivElement>(null);
  const [opened, setOpened] = React.useState(open);
  React.useEffect(() => { if (open) setOpened(true); }, [open]);
  const [stored, setStored] = useLocalStorage<number>(WIDTH_KEY, DEFAULT_WIDTH);
  const saved = typeof stored === "number" && Number.isFinite(stored) ? stored : DEFAULT_WIDTH;
  // 画面に入る幅（本文に MIN_BODY を残す）へ丸めた、実際に表示する幅。
  const rootWidth = useElementWidth(rootRef);
  const width = clampWidth(saved, rootWidth ?? Number.POSITIVE_INFINITY);
  const maxWidth = Math.round(maxWidthFor(rootWidth ?? Number.POSITIVE_INFINITY));
  // 幅は CSS 変数で本文の右余白・パネル幅・上部バーの右余白へ渡す（ドラッグ中は変数だけ書き換える）。
  // シェルの中ではシェルの列に載せ（上部バーも読む）、シェルの外では自分に載せる。
  const dock = useAssistantDock();
  const publish = dock?.publish;
  React.useEffect(() => {
    publish?.({ open, width });
  }, [publish, open, width]);
  React.useEffect(() => () => publish?.(null), [publish]);
  const varRef = dock?.el ?? rootRef;
  return (
    <div
      ref={rootRef}
      // 画面が狭くて保存幅が入らないときは calc 側で本文に MIN_BODY を残す。
      style={dock ? undefined : ({ "--assistant-w": `${width}px` } as React.CSSProperties)}
      className={cn("group/assistant relative flex min-h-0 flex-1", className)}
    >
      <div
        data-testid="assistant-body"
        className={cn(
          "flex min-h-0 min-w-0 flex-1 flex-col transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
          "group-data-[resizing]/assistant:transition-none",
          // アシスタント表示中は本文を「パネルを除いた領域」へ寄せる（PC: 右・スマホ: 上）。
          open &&
            "lg:pr-[min(calc(var(--assistant-w)+28px),calc(100%-352px))] max-lg:pb-[55dvh]",
        )}
      >
        {children}
      </div>
      {overlay ? (
        <div
          className={cn(
            "pointer-events-none absolute inset-x-0 bottom-4 z-20 flex justify-center px-4",
            open && "max-lg:bottom-[calc(min(55dvh,100%)+0.75rem)]",
          )}
        >
          {overlay}
        </div>
      ) : null}
      {open || (keepMounted && opened) ? (
        <AssistantSheet
          open={open}
          onClose={onClose}
          label={label}
          title={title}
          width={width}
          maxWidth={maxWidth}
          rootRef={rootRef}
          varRef={varRef}
          onWidthChange={setStored}
        >
          {panel}
        </AssistantSheet>
      ) : null}
    </div>
  );
}

function AssistantSheet({
  open,
  onClose,
  label,
  title,
  width,
  maxWidth,
  rootRef,
  varRef,
  onWidthChange,
  children,
}: {
  open: boolean;
  onClose: () => void;
  label: string;
  title: string;
  width: number;
  maxWidth: number;
  rootRef: React.RefObject<HTMLDivElement | null>;
  varRef: React.RefObject<HTMLDivElement | null>;
  onWidthChange: (w: number) => void;
  children: React.ReactNode;
}) {
  const [mode, setMode] = React.useState<SheetMode>("half");
  // 幅つまみ（separator）の aria-controls が指すパネルの id。
  const paneId = React.useId();
  const sheetRef = React.useRef<HTMLDivElement>(null);
  // ドラッグ中の状態（再描画を起こさないよう ref で持ち、高さは style を直接書く）。
  const drag = React.useRef<{ id: number; startY: number; startH: number; maxH: number } | null>(
    null,
  );

  const height = mode === "full" ? "100%" : HALF_HEIGHT;
  // PC（右のカード）ではドラッグしない。
  const desktop = useMediaQuery("(min-width: 1024px)");

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (desktop) return;
    // ボタン（閉じる・全画面）の押下はドラッグにしない。
    if ((e.target as HTMLElement).closest("button")) return;
    const el = sheetRef.current;
    const parent = el?.parentElement;
    if (!el || !parent) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = {
      id: e.pointerId,
      startY: e.clientY,
      startH: el.getBoundingClientRect().height,
      maxH: parent.getBoundingClientRect().height,
    };
    el.style.transition = "none";
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const el = sheetRef.current;
    if (!d || !el || d.id !== e.pointerId) return;
    const next = Math.min(d.maxH, Math.max(96, d.startH - (e.clientY - d.startY)));
    el.style.setProperty("--assistant-sheet-h", `${next}px`);
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const el = sheetRef.current;
    if (!d || !el || d.id !== e.pointerId) return;
    drag.current = null;
    el.style.transition = "";
    el.style.removeProperty("--assistant-sheet-h");
    const dy = e.clientY - d.startY;
    if (Math.abs(dy) < 8) {
      // つまみのタップは「半分 ⇄ 全画面」の切替。
      setMode((m) => (m === "full" ? "half" : "full"));
      return;
    }
    if (mode === "half") {
      if (dy < -48) setMode("full");
      else if (dy > 120) onClose();
    } else if (dy > d.maxH * 0.6) {
      onClose();
    } else if (dy > 48) {
      setMode("half");
    }
  };

  return (
    <div
      ref={sheetRef}
      role="complementary"
      aria-label={label}
      id={paneId}
      data-sheet-mode={mode}
      style={{ "--assistant-sheet-base": height, display: open ? undefined : "none" } as React.CSSProperties}
      hidden={!open}
      className={cn(
        "z-20 flex flex-col overflow-hidden bg-card",
        "animate-in fade-in-0 duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
        // PC: 画面の右端に縦いっぱい（上部バーも貫通）に浮かぶ角丸カード（幅は左端のつまみで変える）。
        // 幅は JS 側で本文に MIN_BODY を残すよう丸め済み。
        "lg:fixed lg:inset-y-3 lg:right-3 lg:w-[var(--assistant-w)] lg:rounded-2xl lg:border lg:shadow-lg lg:slide-in-from-right-4",
        // スマホ/タブレット: 下からのシート（非モーダル・本文は上に見えたまま）。
        "max-lg:absolute max-lg:inset-x-0 max-lg:bottom-0 max-lg:slide-in-from-bottom-8",
        "max-lg:h-[var(--assistant-sheet-h,var(--assistant-sheet-base))] max-lg:transition-[height] max-lg:duration-[var(--duration-normal)]",
        "max-lg:rounded-t-2xl max-lg:border-t max-lg:shadow-[0_-10px_30px_-14px_rgb(0_0_0/0.35)]",
        "max-lg:overscroll-contain max-lg:pb-[env(safe-area-inset-bottom)]",
      )}
    >
      {desktop ? (
        <WidthHandle
          controls={paneId}
          width={width}
          maxWidth={maxWidth}
          rootRef={rootRef}
          varRef={varRef}
          onWidthChange={onWidthChange}
        />
      ) : null}
      {/* 見出し（スマホではここ全体がつまみ: ドラッグで高さ・タップで半分⇄全画面） */}
      <div
        className="relative flex shrink-0 touch-none select-none items-center gap-1 px-2 shiki-dash-bottom max-lg:h-12 max-lg:cursor-grab lg:h-11 lg:touch-auto lg:gap-2 lg:px-3"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        <span
          aria-hidden
          className="absolute left-1/2 top-1.5 h-1 w-9 -translate-x-1/2 rounded-full bg-foreground/20 lg:hidden"
        />
        <Sparkles className="ml-1.5 size-4 shrink-0 text-muted-foreground lg:ml-0" aria-hidden />
        <span className="min-w-0 flex-1 truncate pl-1 text-sm font-medium lg:pl-0">{title}</span>
        <button
          type="button"
          onClick={() => setMode((m) => (m === "full" ? "half" : "full"))}
          aria-label={mode === "full" ? "半分の高さに戻す" : "全画面で表示"}
          className="flex size-11 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring active:scale-90 lg:hidden"
        >
          {mode === "full" ? (
            <Minimize2 className="size-[18px]" aria-hidden />
          ) : (
            <Maximize2 className="size-[18px]" aria-hidden />
          )}
        </button>
        <button
          type="button"
          onClick={onClose}
          aria-label="チャットを閉じる"
          className="flex size-11 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring active:scale-90 lg:size-7 lg:rounded-md"
        >
          <X className="size-[18px] lg:size-4" aria-hidden />
        </button>
      </div>
      <div className="min-h-0 flex-1 overscroll-contain">{children}</div>
    </div>
  );
}

/// PC パネルの左端の幅つまみ（WAI-ARIA の window splitter）。
///
/// ドラッグ中は再描画せず、`varRef`（シェルの列）の `--assistant-w` を直接書き換える（本文の右余白・パネル幅・上部バーが
/// 同じ変数で追従する）。離したときに保存する。←/→ キーで 16px ずつ、ダブルクリックで既定幅。
function WidthHandle({
  controls,
  width,
  maxWidth,
  rootRef,
  varRef,
  onWidthChange,
}: {
  /// 幅を変えるパネルの id（aria-controls）。
  controls: string;
  /// 表示中の幅（画面に入るよう丸めた値）。
  width: number;
  maxWidth: number;
  /// 幅の基準（本文＋パネルの領域）。
  rootRef: React.RefObject<HTMLDivElement | null>;
  /// ドラッグ中に `--assistant-w` と `data-resizing` を書く要素。
  varRef: React.RefObject<HTMLDivElement | null>;
  onWidthChange: (w: number) => void;
}) {
  const drag = React.useRef<{ id: number; startX: number; startW: number; rootW: number; w: number } | null>(
    null,
  );
  const [active, setActive] = React.useState(false);
  const rootWidth = () => rootRef.current?.getBoundingClientRect().width ?? window.innerWidth;

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const root = varRef.current;
    if (!root) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const startW = width;
    drag.current = { id: e.pointerId, startX: e.clientX, startW, rootW: rootWidth(), w: startW };
    root.dataset.resizing = "";
    setActive(true);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const root = varRef.current;
    if (!d || !root || d.id !== e.pointerId) return;
    // 左端をつかむので、左へ動かすほど広がる。
    d.w = clampWidth(d.startW - (e.clientX - d.startX), d.rootW);
    root.style.setProperty("--assistant-w", `${d.w}px`);
  };
  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const root = varRef.current;
    if (!d || d.id !== e.pointerId) return;
    drag.current = null;
    setActive(false);
    if (root) delete root.dataset.resizing;
    if (d.w !== d.startW) onWidthChange(d.w);
  };
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const delta =
      e.key === "ArrowLeft" ? KEY_STEP : e.key === "ArrowRight" ? -KEY_STEP : null;
    if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      onWidthChange(clampWidth(e.key === "Home" ? MIN_WIDTH : MAX_WIDTH, rootWidth()));
      return;
    }
    if (delta === null) return;
    e.preventDefault();
    onWidthChange(clampWidth(width + delta, rootWidth()));
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="アシスタントの幅を変更"
      aria-valuemin={Math.min(MIN_WIDTH, maxWidth)}
      aria-valuemax={maxWidth}
      aria-valuenow={width}
      aria-controls={controls}
      tabIndex={0}
      title="ドラッグで幅を変更（ダブルクリックで元の幅）"
      data-testid="assistant-width-handle"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={() => onWidthChange(clampWidth(DEFAULT_WIDTH, rootWidth()))}
      onKeyDown={onKeyDown}
      // つまみは枠の内側 8px。目印の線は枠に重ねて 1 本だけ出す（グローバルのフォーカスリングは
      // 細長い箱になって線が二重に見えるので打ち消し、フォーカス時は線を濃くして示す）。
      className="group/handle absolute inset-y-0 left-0 z-10 flex w-2 cursor-col-resize touch-none select-none justify-start focus-visible:outline-none focus-visible:ring-0 focus-visible:ring-offset-0"
    >
      <span
        aria-hidden
        className={cn(
          "my-6 w-0.5 rounded-full bg-foreground/25 opacity-0 transition-opacity duration-[var(--duration-fast)]",
          "group-hover/handle:opacity-100",
          "group-focus-visible/handle:w-[3px] group-focus-visible/handle:bg-ring group-focus-visible/handle:opacity-100",
          active && "bg-foreground/40 opacity-100",
        )}
      />
    </div>
  );
}

/// 要素の幅（px）を追う。計測前（初回描画）は null。
function useElementWidth(ref: React.RefObject<HTMLElement | null>): number | null {
  const [w, setW] = React.useState<number | null>(null);
  React.useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setW(Math.round(entry.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return w;
}
