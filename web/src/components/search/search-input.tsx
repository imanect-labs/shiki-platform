"use client";

import { Loader2, Search } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { SearchMode } from "@/lib/search";

const MODES: { value: SearchMode; label: string; hint: string }[] = [
  { value: "hybrid", label: "ハイブリッド", hint: "意味＋キーワード（推奨）" },
  { value: "dense", label: "意味", hint: "埋め込みベクトルのみ" },
  { value: "keyword", label: "キーワード", hint: "BM25 全文検索のみ" },
];

/// 検索ボックス＋モード切替＋デバッグトグル。
export function SearchInput({
  query,
  onQueryChange,
  mode,
  onModeChange,
  debug,
  onDebugChange,
  onSubmit,
  loading,
}: {
  query: string;
  onQueryChange: (q: string) => void;
  mode: SearchMode;
  onModeChange: (m: SearchMode) => void;
  debug: boolean;
  onDebugChange: (d: boolean) => void;
  onSubmit: () => void;
  loading: boolean;
}) {
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit();
      }}
    >
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <Input
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            placeholder="例: 上期の拠点別売上は？"
            aria-label="検索クエリ"
            autoFocus
            className="h-11 pl-9 text-base"
          />
        </div>
        <Button type="submit" disabled={loading || !query.trim()} className="h-11 px-5">
          {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : "検索"}
        </Button>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2">
        <div
          role="radiogroup"
          aria-label="検索モード"
          className="inline-flex items-center gap-1 rounded-lg border border-border bg-muted/40 p-1"
        >
          {MODES.map((m) => (
            <button
              key={m.value}
              type="button"
              role="radio"
              aria-checked={mode === m.value}
              title={m.hint}
              onClick={() => onModeChange(m.value)}
              className={cn(
                "rounded-md px-3 py-1 text-xs font-medium transition-colors",
                mode === m.value
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {m.label}
            </button>
          ))}
        </div>

        <label className="flex cursor-pointer select-none items-center gap-2 text-xs text-muted-foreground">
          <input
            type="checkbox"
            checked={debug}
            onChange={(e) => onDebugChange(e.target.checked)}
            className="size-3.5 accent-primary"
          />
          デバッグ情報（各段の絞り込み件数）を表示
        </label>
      </div>
    </form>
  );
}
