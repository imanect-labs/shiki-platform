"use client";

/// 文書エディタ（ノート・スライド・CSV・Office）の AI アシスタントの共通コンテナ。
///
/// - lg 以上: 本文の右に浮かぶ角丸カード（本文は `lg:pr-[28rem]` でパネルを避ける）。
/// - lg 未満: 下からの**非モーダル**なシート。初期は約 55dvh（半分）で、本文はシートの上に
///   見えたまま選択・編集できる（本文側にシートの高さぶんの下余白を入れる）。つまみのドラッグ
///   または ⤢ ボタンで「半分 ⇄ 全画面」、半分から大きく下へ引くと閉じる。
///
/// 開くたびに半分から始める（`?thread=` 付きで開いたとき・下書き保存直後も全画面にしない）。
/// パネルの中身（会話）は 1 つだけマウントし、幅に応じてクラスで形を変える（会話を二重に
/// 張らない）。

import { Maximize2, Minimize2, Sparkles, X } from "lucide-react";
import * as React from "react";

import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";

type SheetMode = "half" | "full";

/// 半分のときのシートの高さ（本文の下余白と同じ値）。
const HALF_HEIGHT = "min(55dvh, 100%)";

export function AssistantLayout({
  open,
  onClose,
  label,
  title = "アシスタント",
  panel,
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
  /// パネルの中身（会話）。open のときだけ描く。
  panel: React.ReactNode;
  /// 本文の下端に重ねる案内（引用ハイライトのヒント等）。シートが開いていればその上に出す。
  overlay?: React.ReactNode;
  className?: string;
  /// 本文。`min-h-0 flex-1` の要素を置く（スクロールは本文側で持つ）。
  children: React.ReactNode;
}) {
  return (
    <div className={cn("relative flex min-h-0 flex-1", className)}>
      <div
        className={cn(
          "flex min-h-0 min-w-0 flex-1 flex-col transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
          // アシスタント表示中は本文を「パネルを除いた領域」へ寄せる（PC: 右・スマホ: 上）。
          open && "lg:pr-[28rem] max-lg:pb-[55dvh]",
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
      {open ? (
        <AssistantSheet onClose={onClose} label={label} title={title}>
          {panel}
        </AssistantSheet>
      ) : null}
    </div>
  );
}

function AssistantSheet({
  onClose,
  label,
  title,
  children,
}: {
  onClose: () => void;
  label: string;
  title: string;
  children: React.ReactNode;
}) {
  const [mode, setMode] = React.useState<SheetMode>("half");
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
      data-sheet-mode={mode}
      style={{ "--assistant-sheet-base": height } as React.CSSProperties}
      className={cn(
        "z-20 flex flex-col overflow-hidden bg-card",
        "animate-in fade-in-0 duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
        // PC: 本文の右に浮かぶ角丸カード。
        "lg:absolute lg:inset-y-3 lg:right-3 lg:w-[min(420px,calc(100%-1.5rem))] lg:rounded-2xl lg:border lg:shadow-lg lg:slide-in-from-right-4",
        // スマホ/タブレット: 下からのシート（非モーダル・本文は上に見えたまま）。
        "max-lg:absolute max-lg:inset-x-0 max-lg:bottom-0 max-lg:slide-in-from-bottom-8",
        "max-lg:h-[var(--assistant-sheet-h,var(--assistant-sheet-base))] max-lg:transition-[height] max-lg:duration-[var(--duration-normal)]",
        "max-lg:rounded-t-2xl max-lg:border-t max-lg:shadow-[0_-10px_30px_-14px_rgb(0_0_0/0.35)]",
        "max-lg:overscroll-contain max-lg:pb-[env(safe-area-inset-bottom)]",
      )}
    >
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
