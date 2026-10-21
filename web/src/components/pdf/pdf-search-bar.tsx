"use client";

/// PDF ビューアの文書内検索バー（⌘F で開く・Enter で次・Shift+Enter で前・Esc で閉じる）。
import * as React from "react";
import { ChevronDown, ChevronUp, Loader2, Search, X } from "lucide-react";

import { cn } from "@/lib/utils";

export function PdfSearchBar({
  inputRef,
  query,
  onQueryChange,
  total,
  index,
  scanning,
  onNext,
  onPrev,
  onClose,
  className,
}: {
  inputRef: React.RefObject<HTMLInputElement | null>;
  query: string;
  onQueryChange: (q: string) => void;
  total: number;
  /// 今の一致の番号（0 始まり）。一致が無ければ -1。
  index: number;
  /// まだ全ページを読み終えていない。
  scanning: boolean;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
  className?: string;
}) {
  const hasQuery = query.trim().length > 0;
  const status = !hasQuery
    ? ""
    : total > 0
      ? `${index + 1} / ${total}${scanning ? "+" : ""}`
      : scanning
        ? "検索中…"
        : "見つかりません";
  return (
    <div
      role="search"
      className={cn(
        "flex items-center gap-1 rounded-xl bg-background/95 p-1 pl-2.5 shadow-lg ring-1 ring-border backdrop-blur focus-within:ring-2 focus-within:ring-ring",
        className,
      )}
      data-testid="pdf-search"
    >
      {scanning && hasQuery ? (
        <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
      ) : (
        <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
      )}
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => onQueryChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            if (e.shiftKey) onPrev();
            else onNext();
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
        placeholder="文書内を検索"
        aria-label="文書内を検索"
        enterKeyHint="search"
        spellCheck={false}
        autoComplete="off"
        // 入力欄の枠はバー全体が担う（欄だけにフォーカスリングを重ねない）。
        className="h-8 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-0 focus-visible:ring-offset-0 max-md:text-base"
        data-testid="pdf-search-input"
      />
      <span
        className={cn(
          "shrink-0 px-1 text-xs tabular-nums",
          hasQuery && total === 0 && !scanning ? "text-destructive" : "text-muted-foreground",
        )}
        aria-live="polite"
        data-testid="pdf-search-status"
      >
        {status}
      </span>
      <SearchButton label="前の一致（Shift+Enter）" onClick={onPrev} disabled={total === 0}>
        <ChevronUp aria-hidden />
      </SearchButton>
      <SearchButton label="次の一致（Enter）" onClick={onNext} disabled={total === 0} testId="pdf-search-next">
        <ChevronDown aria-hidden />
      </SearchButton>
      <SearchButton label="検索を閉じる（Esc）" onClick={onClose}>
        <X aria-hidden />
      </SearchButton>
    </div>
  );
}

function SearchButton({
  label,
  onClick,
  disabled,
  testId,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  testId?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      data-testid={testId}
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-lg text-foreground/70 outline-none",
        "hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
        "disabled:pointer-events-none disabled:opacity-35 pointer-coarse:size-10 [&_svg]:size-4",
      )}
    >
      {children}
    </button>
  );
}
