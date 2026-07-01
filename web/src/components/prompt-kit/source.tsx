// prompt-kit (https://www.prompt-kit.com) の Source 相当を本リポジトリ向けに実装。MIT License。
// doc_search が返した引用（権限内のソース）を番号付きチップで表示する。チップのクリックで
// スニペットをその場でプレビューできる。FR-3「権限内のソースのみ」を体現。
"use client";

import * as React from "react";
import { ChevronDown, FileText } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Citation } from "@/lib/chat-api";

function label(c: Citation): string {
  return c.heading_path && c.heading_path.length > 0
    ? c.heading_path[c.heading_path.length - 1]
    : "ドキュメント";
}

export function Sources({ citations }: { citations: Citation[] }) {
  const [openIdx, setOpenIdx] = React.useState<number | null>(null);
  if (citations.length === 0) return null;

  return (
    <div className="mt-3 border-t border-border/60 pt-2.5">
      <div className="mb-1.5 flex items-center gap-1.5 text-[12px] font-medium text-muted-foreground">
        <FileText className="size-3.5" aria-hidden />
        参照したソース（{citations.length}）
      </div>
      <div className="flex flex-wrap gap-1.5">
        {citations.map((c, i) => {
          const open = openIdx === i;
          return (
            <button
              key={c.chunk_id}
              type="button"
              onClick={() => setOpenIdx(open ? null : i)}
              aria-label="スニペットをプレビュー"
              aria-expanded={open}
              className={cn(
                "group inline-flex max-w-[240px] items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12px] transition-colors",
                open
                  ? "border-ring/40 bg-secondary text-foreground"
                  : "border-border bg-card text-foreground/85 hover:border-ring/30 hover:text-foreground",
              )}
            >
              <span
                style={{ ["--season" as string]: seasonVar(i) }}
                className="flex size-4 shrink-0 items-center justify-center rounded-full bg-[var(--season)]/15 text-[10px] font-semibold text-[var(--season)]"
              >
                {i + 1}
              </span>
              <span className="truncate">{label(c)}</span>
              <ChevronDown
                className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")}
                aria-hidden
              />
            </button>
          );
        })}
      </div>
      {openIdx !== null && citations[openIdx] ? (
        <div className="mt-2 rounded-lg border border-border bg-muted/40 p-3 text-[13px] leading-relaxed text-foreground/85">
          {citations[openIdx].heading_path && citations[openIdx].heading_path!.length > 0 ? (
            <div className="mb-1 text-[11px] text-muted-foreground">
              {citations[openIdx].heading_path!.join(" › ")}
              {citations[openIdx].page != null ? ` ・ p.${citations[openIdx].page}` : ""}
            </div>
          ) : null}
          <p className="line-clamp-6 whitespace-pre-wrap">{citations[openIdx].snippet}</p>
        </div>
      ) : null}
    </div>
  );
}
