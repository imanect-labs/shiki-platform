"use client";

/// 左ペインのノードパレット（二次導線・dnd でキャンバスへ配置）。
///
/// 主導線はノードの尻尾プラスボタン（node-card.tsx）。パレットは全体を見渡して
/// 「何ができるか」を掴む場でもあるため、カテゴリ見出し＋検索を常設する。

import * as React from "react";
import { GripVertical, Search } from "lucide-react";

import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { NodeCatalogEntry } from "@/generated/workflow-ir";
import { nodeIcon } from "./icons";
import { categoryVar } from "../category-accent";

export const PALETTE_DND_TYPE = "application/x-shiki-node-type";

function groupByCategory(entries: readonly NodeCatalogEntry[]) {
  const groups = new Map<string, { label: string; items: NodeCatalogEntry[] }>();
  for (const e of entries) {
    const g = groups.get(e.category) ?? { label: e.category_label_ja, items: [] };
    g.items.push(e);
    groups.set(e.category, g);
  }
  return [...groups.values()];
}

export function Palette() {
  const [query, setQuery] = React.useState("");
  const q = query.trim().toLowerCase();
  const filtered = NODE_CATALOG.filter(
    (e) =>
      !q ||
      e.label_ja.toLowerCase().includes(q) ||
      e.description_ja.toLowerCase().includes(q),
  );
  const groups = groupByCategory(filtered.filter((e) => e.available));
  const upcoming = filtered.filter((e) => !e.available);

  return (
    <aside
      className="flex h-full w-64 flex-col overflow-hidden rounded-xl border bg-card shadow-lg"
      aria-label="ブロック一覧"
    >
      <div className="shiki-dash-bottom px-3 py-3">
        <h2 className="text-sm font-semibold">ブロック</h2>
        <p className="mt-0.5 text-xs text-muted-foreground">
          ドラッグして置くか、ノードの＋から追加
        </p>
        <div className="relative mt-2">
          <Search
            className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="検索…"
            className="h-8 pl-8 text-sm"
            aria-label="ブロックを検索"
          />
        </div>
      </div>
      <div className="flex-1 overflow-y-auto px-2 py-2 scrollbar-subtle">
        {groups.map((group) => (
          <div key={group.label} className="mb-3">
            <p className="px-1.5 py-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              {group.label}
            </p>
            <ul className="space-y-0.5">
              {group.items.map((entry) => {
                const Icon = nodeIcon(entry.type);
                return (
                  <li key={entry.type}>
                    <div
                      draggable
                      onDragStart={(e) => {
                        e.dataTransfer.setData(PALETTE_DND_TYPE, entry.type);
                        e.dataTransfer.effectAllowed = "move";
                      }}
                      className={cn(
                        "flex cursor-grab items-center gap-2 rounded-md border border-transparent px-1.5 py-1.5",
                        "transition-colors duration-fast hover:border-border hover:bg-accent/60 active:cursor-grabbing",
                      )}
                      title={entry.description_ja}
                    >
                      <GripVertical className="size-3.5 shrink-0 text-muted-foreground/50" aria-hidden />
                      <Icon className="size-4 shrink-0" style={{ color: categoryVar(entry.category) }} aria-hidden />
                      <span className="truncate text-sm">{entry.label_ja}</span>
                    </div>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
        {upcoming.length > 0 ? (
          <div className="mb-2 border-t pt-2">
            <p className="px-1.5 py-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              近日対応
            </p>
            <ul className="space-y-0.5">
              {upcoming.slice(0, 12).map((entry) => {
                const Icon = nodeIcon(entry.type);
                return (
                  <li key={entry.type}>
                    <div
                      className="flex items-center gap-2 rounded-md px-1.5 py-1.5 opacity-45"
                      title="近日対応予定です"
                    >
                      <span className="size-3.5 shrink-0" />
                      <Icon className="size-4 shrink-0" aria-hidden />
                      <span className="truncate text-sm">{entry.label_ja}</span>
                    </div>
                  </li>
                );
              })}
            </ul>
          </div>
        ) : null}
      </div>
    </aside>
  );
}
