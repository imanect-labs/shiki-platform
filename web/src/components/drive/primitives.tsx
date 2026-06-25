"use client";

import * as React from "react";
import { ChevronRight, File as FileIcon, Folder, Home, Loader2 } from "lucide-react";

import type { CrumbResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

/// ノード種別アイコン（フォルダは藍、ファイルは控えめ）。
export function NodeIcon({ kind, className }: { kind: string; className?: string }) {
  if (kind === "folder") {
    return <Folder className={cn("size-5 text-primary", className)} aria-hidden />;
  }
  return <FileIcon className={cn("size-5 text-muted-foreground", className)} aria-hidden />;
}

/// パンくず（root→自身）。各要素クリックでそのフォルダへ遷移する。
export function Breadcrumbs({
  crumbs,
  onNavigate,
}: {
  crumbs: CrumbResponse[];
  /// `null` でルートへ。
  onNavigate: (id: string | null) => void;
}) {
  return (
    <nav aria-label="パンくず" className="flex min-w-0 items-center gap-1 text-sm">
      <button
        type="button"
        onClick={() => onNavigate(null)}
        className="flex shrink-0 items-center gap-1 rounded-md px-1.5 py-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      >
        <Home className="size-4" aria-hidden />
        <span>ドライブ</span>
      </button>
      {crumbs.map((c, i) => {
        const last = i === crumbs.length - 1;
        return (
          <React.Fragment key={c.id}>
            <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" aria-hidden />
            <button
              type="button"
              onClick={() => onNavigate(c.id)}
              aria-current={last ? "page" : undefined}
              className={cn(
                "truncate rounded-md px-1.5 py-1 transition-colors hover:bg-accent",
                last ? "font-medium text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {c.name}
            </button>
          </React.Fragment>
        );
      })}
    </nav>
  );
}

/// 一覧フッタのローディング行（無限スクロールの sentinel と併用）。
export function LoadingRow({ label = "読み込み中…" }: { label?: string }) {
  return (
    <div className="flex items-center justify-center gap-2 py-4 text-sm text-muted-foreground">
      <Loader2 className="size-4 animate-spin" aria-hidden />
      <span>{label}</span>
    </div>
  );
}
