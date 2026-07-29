"use client";

/// スラッシュコマンドの補完メニュー（issue #387）。
///
/// 入力欄の先頭で `/` を打つと出る。↑↓ で移動・Enter/Tab で確定・Esc で閉じる。
/// 候補は `GET /skills/catalog`（モデルが見ているカタログと同一の源）由来で、
/// **コマンド宣言を持つスキルだけ**が並ぶ。
///
/// 見た目は既存のドロップダウン（`components/ui/dropdown-menu`）の作法に合わせる:
/// 選択は塗り（`bg-accent`）で示し、`border-primary` の枠は使わない。

import * as React from "react";

import { Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import type { SlashSuggestion } from "@/lib/slash-command";

export function SlashCommandMenu({
  suggestions,
  activeIndex,
  onPick,
  onHover,
}: {
  suggestions: SlashSuggestion[];
  activeIndex: number;
  onPick: (s: SlashSuggestion) => void;
  onHover: (index: number) => void;
}) {
  const listRef = React.useRef<HTMLDivElement | null>(null);

  // キーボード移動でアクティブ項目が隠れないよう追従させる。
  React.useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-index="${activeIndex}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (suggestions.length === 0) return null;

  return (
    <div
      ref={listRef}
      role="listbox"
      aria-label="スキルコマンド"
      data-testid="slash-command-menu"
      className={cn(
        "absolute bottom-full left-0 z-50 mb-2 max-h-72 w-full max-w-lg overflow-y-auto",
        "rounded-xl border border-border/60 bg-popover p-1.5 shadow-md",
      )}
    >
      {suggestions.map((s, i) => (
        <button
          key={s.key}
          type="button"
          role="option"
          aria-selected={i === activeIndex}
          data-index={i}
          data-testid="slash-command-option"
          onMouseEnter={() => onHover(i)}
          // blur でメニューが閉じる前に確定させる（mousedown で拾う）。
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(s);
          }}
          className={cn(
            "flex w-full items-start gap-2.5 rounded-lg px-2.5 py-2 text-left",
            "transition-colors duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
            i === activeIndex ? "bg-accent" : "hover:bg-accent/50",
          )}
        >
          <Sparkles className="mt-0.5 size-3.5 shrink-0 text-primary" aria-hidden />
          <span className="min-w-0 flex-1">
            <span className="flex min-w-0 items-baseline gap-1.5">
              <span className="truncate text-sm font-medium text-foreground">/{s.token}</span>
              <span className="shrink-0 text-[11px] text-muted-foreground">{s.skillName}</span>
            </span>
            <span className="mt-0.5 block line-clamp-2 text-[12px] leading-relaxed text-muted-foreground">
              {s.summary}
            </span>
          </span>
        </button>
      ))}
    </div>
  );
}

/// 確定したコマンドのピル（入力欄の左に置く・ChatGPT 相当の「確定が分かる」表示）。
export function SlashCommandPill({
  label,
  onClear,
}: {
  label: string;
  onClear: () => void;
}) {
  return (
    <span
      data-testid="slash-command-pill"
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 rounded-md bg-accent px-2 py-1",
        "text-[13px] font-medium text-foreground",
      )}
    >
      <Sparkles className="size-3.5 text-primary" aria-hidden />
      /{label}
      <button
        type="button"
        onClick={onClear}
        aria-label="コマンドを外す"
        className="text-muted-foreground transition-colors hover:text-foreground"
      >
        ×
      </button>
    </span>
  );
}
