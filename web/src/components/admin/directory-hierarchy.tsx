"use client";

import * as React from "react";
import { ChevronDown, ChevronRight, Folder, Users } from "lucide-react";
import { cn } from "@/lib/utils";
import { AD_GROUPS, type Mapping } from "./mock-directory-model";

type Item = { id: string; label: string; group?: string; children: Item[] };
function ouItems(): Item[] {
  const root: Item[] = [];
  for (const group of AD_GROUPS) {
    let children = root;
    for (const part of group.ou.split("/")) {
      let folder = children.find((item) => !item.group && item.label === part);
      if (!folder) { folder = { id: part, label: part, children: [] }; children.push(folder); }
      children = folder.children;
    }
    children.push({ id: group.id, label: group.name, group: group.id, children: [] });
  }
  return [{ id: "domain", label: "example.co.jp", children: root }];
}
function nestedItems(): Item[] {
  function item(id: string, path: string[]): Item {
    const group = AD_GROUPS.find((g) => g.id === id)!;
    return { id, group: id, label: group.name, children: path.includes(id) ? [] : group.groups.map((child) => item(child, [...path, id])) };
  }
  return AD_GROUPS.filter((g) => !AD_GROUPS.some((parent) => parent.groups.includes(g.id))).map((g) => item(g.id, []));
}
const OU_ITEMS = ouItems();
const NESTED_ITEMS = nestedItems();

/** 開閉はボタン、選択は別ボタン。通常のTab移動を使うネストリスト。 */
export function DirectoryHierarchy({ mode, query, unmappedOnly, selected, mapping, onSelect }: {
  mode: "ou" | "membership"; query: string; unmappedOnly: boolean;
  selected: string; mapping: Mapping; onSelect: (id: string) => void;
}) {
  const [closed, setClosed] = React.useState<Set<string>>(new Set());
  const matches = (item: Item): boolean => {
    const group = AD_GROUPS.find((g) => g.id === item.group);
    return !!group && (!unmappedOnly || !mapping[group.id].length) && `${group.name} ${group.ou}`.toLowerCase().includes(query.toLowerCase());
  };
  const visible = (item: Item): boolean => matches(item) || item.children.some(visible);
  function render(items: Item[], path = "root"): React.ReactNode {
    return <ul className={path === "root" ? "space-y-0.5" : "ml-2 space-y-0.5 border-l border-border/60 pl-1"}>{items.filter(visible).map((item) => {
      const key = `${path}/${item.id}`;
      const expanded = query || unmappedOnly ? true : !closed.has(key);
      return <li key={key}>
        <div className={cn("flex min-h-9 items-center gap-1 rounded-md pr-2 text-[13px] pointer-coarse:min-h-11", item.group === selected && "bg-accent")}>
          {item.children.length ? <button type="button" aria-label={`${item.label}を${expanded ? "折りたたむ" : "展開する"}`} aria-expanded={!!expanded} onClick={() => setClosed((prev) => { const next = new Set(prev); if (next.has(key)) next.delete(key); else next.add(key); return next; })} className="flex size-8 shrink-0 items-center justify-center rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">{expanded ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}</button> : <span className="w-8 shrink-0" />}
          {item.group ? <button type="button" aria-pressed={selected === item.group} onClick={() => onSelect(item.group!)} className="flex min-w-0 flex-1 items-center gap-2 self-stretch rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><Users className="size-4 shrink-0 text-muted-foreground" aria-hidden /><span className="min-w-0 break-words">{item.label}</span><span className={cn("ml-auto shrink-0 text-[10px]", mapping[item.group].length ? "text-muted-foreground" : "text-amber-700 dark:text-amber-400")}>{mapping[item.group].length ? "対応済" : "未対応"}</span></button> : <span className="flex items-center gap-2 font-medium"><Folder className="size-4 text-muted-foreground" aria-hidden />{item.label}<span className="text-[10px] font-normal text-muted-foreground">{item.id === "domain" ? "ドメイン" : "OU"}</span></span>}
        </div>
        {expanded && item.children.length > 0 && render(item.children, key)}
      </li>;
    })}</ul>;
  }
  const items = mode === "ou" ? OU_ITEMS : NESTED_ITEMS;
  return <>{items.some(visible) ? render(items) : <p className="p-6 text-center text-sm text-muted-foreground">該当するグループはありません。</p>}</>;
}
