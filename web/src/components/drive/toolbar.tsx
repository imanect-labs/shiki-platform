"use client";

import { ArrowDown, ArrowUp, Check, LayoutGrid, List as ListIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { SortField } from "@/lib/storage";
import { cn } from "@/lib/utils";

export type ViewMode = "list" | "grid";

/// 並べ替えメニューの選択肢（ラベルは列見出しと統一）。
export const SORT_OPTIONS: { field: SortField; label: string }[] = [
  { field: "name", label: "名前" },
  { field: "updated", label: "更新日時" },
  { field: "size", label: "サイズ" },
];

/// 並べ替え（「名前 ↑」）。トリガに現在の列と向きを出し、メニューで列と昇降を選ぶ。
/// PC のツールバーとスマホの見出し横で共用する。
export function SortMenu({
  sort,
  desc,
  onSortChange,
  onDescChange,
  className,
}: {
  sort: SortField;
  desc: boolean;
  onSortChange: (field: SortField) => void;
  onDescChange: (desc: boolean) => void;
  className?: string;
}) {
  const label = SORT_OPTIONS.find((o) => o.field === sort)?.label ?? "名前";
  const Arrow = desc ? ArrowDown : ArrowUp;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className={cn("gap-1 rounded-lg px-2.5 text-muted-foreground hover:text-foreground", className)}
          aria-label={`並べ替え（${label}・${desc ? "降順" : "昇順"}）`}
        >
          <span>{label}</span>
          <Arrow className="size-4" aria-hidden />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-44">
        <DropdownMenuLabel>並べ替え</DropdownMenuLabel>
        {SORT_OPTIONS.map((o) => (
          <DropdownMenuItem key={o.field} onSelect={() => onSortChange(o.field)}>
            <Check className={cn("size-4", sort === o.field ? "opacity-100" : "opacity-0")} aria-hidden />
            {o.label}
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => onDescChange(false)}>
          <Check className={cn("size-4", !desc ? "opacity-100" : "opacity-0")} aria-hidden />
          昇順
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onDescChange(true)}>
          <Check className={cn("size-4", desc ? "opacity-100" : "opacity-0")} aria-hidden />
          降順
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// 表示切替（一覧/グリッド）。タッチでは 1 ボタン 40px（枠込みで 44px）にする。
export function ViewToggle({
  view,
  onChange,
  className,
}: {
  view: ViewMode;
  onChange: (v: ViewMode) => void;
  className?: string;
}) {
  const btn = (v: ViewMode, label: string, Icon: typeof ListIcon) => (
    <button
      type="button"
      onClick={() => onChange(v)}
      aria-label={label}
      aria-pressed={view === v}
      className={cn(
        "flex size-7 items-center justify-center rounded-md outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-10",
        view === v ? "bg-accent text-foreground" : "text-muted-foreground hover:text-foreground",
      )}
    >
      <Icon className="size-4" aria-hidden />
    </button>
  );
  return (
    <div className={cn("flex shrink-0 items-center rounded-lg border border-border p-0.5", className)}>
      {btn("list", "一覧表示", ListIcon)}
      {btn("grid", "グリッド表示", LayoutGrid)}
    </div>
  );
}
