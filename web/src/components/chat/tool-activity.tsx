"use client";

import { Loader2, Search, Check, Terminal } from "lucide-react";

import { cn } from "@/lib/utils";

export type ToolActivityItem = {
  id: string;
  name: string;
  running: boolean;
  /// ツール入力（doc_search なら `{ query }`）。CoT で検索クエリを見せるのに使う。
  input?: unknown;
};

const TOOL_LABEL: Record<string, { label: string; icon: typeof Search }> = {
  doc_search: { label: "社内文書を検索", icon: Search },
  code_interpreter: { label: "コードを実行", icon: Terminal },
};

/// エージェントのツール実行（Chain of Thought）を可視化する。検索中はスピナー、完了はチェック。
export function ToolActivity({ items }: { items: ToolActivityItem[] }) {
  if (items.length === 0) return null;
  return (
    <div className="mb-2 flex flex-col gap-1.5">
      {items.map((it) => {
        const meta = TOOL_LABEL[it.name] ?? { label: it.name, icon: Search };
        const Icon = meta.icon;
        return (
          <div
            key={it.id}
            className={cn(
              "inline-flex w-fit items-center gap-2 rounded-full border border-border bg-card px-3 py-1 text-[13px]",
              it.running ? "text-foreground" : "text-muted-foreground",
            )}
          >
            <Icon className="size-3.5 text-primary" aria-hidden />
            <span>{meta.label}</span>
            {it.running ? (
              <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-hidden />
            ) : (
              <Check className="size-3.5 text-primary" aria-hidden />
            )}
          </div>
        );
      })}
    </div>
  );
}
