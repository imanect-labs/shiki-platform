"use client";

/// ノード追加メニュー（プラスボタン/エッジ挿入から開く・カテゴリ別・検索可）。
///
/// カタログは codegen（NODE_CATALOG・日本語ラベル/説明/カテゴリの単一定義）。
/// `available=false`（予約語彙）は「近日対応」でグレーアウトし、存在は見せて期待値を作る。

import * as React from "react";
import { Search } from "lucide-react";

import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { NodeCatalogEntry } from "@/generated/workflow-ir";
import { nodeIcon } from "./icons";

type Props = {
  onPick: (nodeType: string) => void;
  /// 呼び出し元の文脈ラベル（例: 「read_1 の後ろに追加」）。
  contextLabel?: string;
};

function groupByCategory(entries: readonly NodeCatalogEntry[]) {
  const groups = new Map<string, { label: string; items: NodeCatalogEntry[] }>();
  for (const e of entries) {
    const g = groups.get(e.category) ?? { label: e.category_label_ja, items: [] };
    g.items.push(e);
    groups.set(e.category, g);
  }
  return [...groups.values()];
}

export function AddNodeMenu({ onPick, contextLabel }: Props) {
  const [query, setQuery] = React.useState("");
  const inputRef = React.useRef<HTMLInputElement>(null);

  React.useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const q = query.trim().toLowerCase();
  const filtered = NODE_CATALOG.filter(
    (e) =>
      !q ||
      e.label_ja.toLowerCase().includes(q) ||
      e.description_ja.toLowerCase().includes(q) ||
      e.type.toLowerCase().includes(q),
  );
  // 利用可能なものを先に・予約語彙（近日対応）は末尾のカテゴリ群として見せる。
  const availableGroups = groupByCategory(filtered.filter((e) => e.available));
  const upcoming = filtered.filter((e) => !e.available);

  return (
    <div className="flex max-h-96 w-80 flex-col">
      {contextLabel ? (
        <p className="px-1 pb-2 text-xs text-muted-foreground">{contextLabel}</p>
      ) : null}
      <div className="relative">
        <Search
          className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          aria-hidden
        />
        <Input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="ブロックを検索…"
          className="h-9 pl-8"
          aria-label="ブロックを検索"
        />
      </div>
      <div className="mt-2 flex-1 overflow-y-auto pr-1 scrollbar-subtle">
        {availableGroups.map((group) => (
          <div key={group.label} className="mb-2">
            <p className="px-1 py-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              {group.label}
            </p>
            <ul>
              {group.items.map((entry) => {
                const Icon = nodeIcon(entry.type);
                return (
                  <li key={entry.type}>
                    <button
                      type="button"
                      onClick={() => onPick(entry.type)}
                      className={cn(
                        "flex w-full items-start gap-2.5 rounded-md px-2 py-1.5 text-left",
                        "transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      )}
                    >
                      <Icon className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
                      <span className="min-w-0">
                        <span className="block text-sm font-medium leading-5">
                          {entry.label_ja}
                        </span>
                        <span className="block truncate text-xs text-muted-foreground">
                          {entry.description_ja}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
        {upcoming.length > 0 ? (
          <div className="mb-1 border-t pt-2">
            <p className="px-1 py-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              近日対応
            </p>
            <ul>
              {upcoming.map((entry) => {
                const Icon = nodeIcon(entry.type);
                return (
                  <li key={entry.type}>
                    <div
                      className="flex w-full cursor-not-allowed items-start gap-2.5 rounded-md px-2 py-1.5 opacity-45"
                      aria-disabled
                      title="近日対応予定です"
                    >
                      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden />
                      <span className="min-w-0">
                        <span className="block text-sm font-medium leading-5">
                          {entry.label_ja}
                        </span>
                        <span className="block truncate text-xs text-muted-foreground">
                          {entry.description_ja}
                        </span>
                      </span>
                    </div>
                  </li>
                );
              })}
            </ul>
          </div>
        ) : null}
        {filtered.length === 0 ? (
          <p className="px-1 py-6 text-center text-sm text-muted-foreground">
            「{query}」に一致するブロックはありません
          </p>
        ) : null}
      </div>
    </div>
  );
}
