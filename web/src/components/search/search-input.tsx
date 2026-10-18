"use client";

import * as React from "react";
import { Loader2, Search } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { SearchMode } from "@/lib/search";

const MODES: { value: SearchMode; label: string; hint: string }[] = [
  { value: "hybrid", label: "ハイブリッド", hint: "意味＋キーワード（推奨）" },
  { value: "dense", label: "意味", hint: "埋め込みベクトルのみ" },
  { value: "keyword", label: "キーワード", hint: "BM25 全文検索のみ" },
];

/// 検索ボックス＋モード切替＋デバッグトグル。
/// モードの説明は title（ホバー）だけだとタッチで読めないため、選択中のものを下に文字で出す。
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
  const inputRef = React.useRef<HTMLInputElement>(null);
  // マウスの端末だけ開いた時に入力へフォーカスする（タッチで開いた瞬間にキーボードを出さない）。
  React.useEffect(() => {
    if (window.matchMedia("(pointer: fine)").matches) inputRef.current?.focus();
  }, []);
  const activeHint = MODES.find((m) => m.value === mode)?.hint;
  const debugId = React.useId();

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        // 送信でキーボードを閉じ、結果を見えるようにする（スマホ）。
        inputRef.current?.blur();
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
            ref={inputRef}
            type="search"
            enterKeyHint="search"
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            placeholder="例: 上期の拠点別売上は？"
            aria-label="検索クエリ"
            className="h-11 pl-9 text-base"
          />
        </div>
        <Button type="submit" disabled={loading || !query.trim()} className="h-11 px-5">
          {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : "検索"}
        </Button>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div className="flex min-w-0 flex-col gap-1 max-sm:w-full">
          <div
            role="radiogroup"
            aria-label="検索モード"
            className="inline-flex items-center gap-1 rounded-lg border border-border bg-muted/40 p-1 max-sm:flex"
          >
            {MODES.map((m) => (
              <button
                key={m.value}
                type="button"
                role="radio"
                aria-checked={mode === m.value}
                aria-describedby={mode === m.value ? `${debugId}-hint` : undefined}
                title={m.hint}
                onClick={() => onModeChange(m.value)}
                className={cn(
                  "rounded-md px-3 py-1 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  "pointer-coarse:min-h-10 pointer-coarse:px-3.5 pointer-coarse:text-[13px] max-sm:flex-1",
                  mode === m.value
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {m.label}
              </button>
            ))}
          </div>
          {activeHint ? (
            <p id={`${debugId}-hint`} className="px-1 text-[11.5px] text-muted-foreground">
              {activeHint}
            </p>
          ) : null}
        </div>

        <label
          htmlFor={debugId}
          className="flex min-h-10 cursor-pointer select-none items-center gap-2.5 text-xs text-muted-foreground"
        >
          <Switch id={debugId} checked={debug} onCheckedChange={onDebugChange} />
          デバッグ情報（各段の絞り込み件数）を表示
        </label>
      </div>
    </form>
  );
}
