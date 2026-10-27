"use client";

import * as React from "react";

import { cn } from "@/lib/utils";
import { HOME_MODES, type HomeMode } from "@/lib/home-modes";

/// ホームの入力欄の上に置くモード切替（チャット／社内検索／エージェント／作成・#964）。
/// 形はサイドバーの履歴の切替と同じ丸いピル。←→ で隣のモードへ移れる（tablist の作法）。
export function HomeModeTabs({
  value,
  onChange,
}: {
  value: HomeMode;
  onChange: (mode: HomeMode) => void;
}) {
  const refs = React.useRef<(HTMLButtonElement | null)[]>([]);

  const onKeyDown = (e: React.KeyboardEvent, index: number) => {
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (step === 0) return;
    e.preventDefault();
    const next = (index + step + HOME_MODES.length) % HOME_MODES.length;
    onChange(HOME_MODES[next].key);
    refs.current[next]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-label="モード"
      className="inline-flex max-w-full gap-0.5 rounded-full border border-border bg-muted p-[3px]"
    >
      {HOME_MODES.map((m, i) => {
        const active = m.key === value;
        const Icon = m.icon;
        return (
          <button
            key={m.key}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="tab"
            aria-selected={active}
            tabIndex={active ? 0 : -1}
            data-testid={`home-mode-${m.key}`}
            onClick={() => onChange(m.key)}
            onKeyDown={(e) => onKeyDown(e, i)}
            className={cn(
              "inline-flex h-8 items-center gap-1.5 whitespace-nowrap rounded-full px-3 text-[13px] transition-colors sm:px-3.5",
              "outline-none focus-visible:ring-2 focus-visible:ring-ring",
              active
                ? "bg-card text-foreground shadow-xs ring-1 ring-border"
                : "text-foreground/65 hover:text-foreground",
            )}
          >
            <Icon className="hidden size-[15px] sm:block" aria-hidden />
            {m.label}
          </button>
        );
      })}
    </div>
  );
}
