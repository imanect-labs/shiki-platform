"use client";

import * as React from "react";
import Link from "next/link";
import {
  Bot,
  FileSpreadsheet,
  FileText,
  MessageSquareText,
  NotebookPen,
  Presentation,
  type LucideIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";
import type { Thread } from "@/lib/chat-api";
import type { NodeResponse } from "@/lib/storage";
import { relativeTime } from "@/lib/format";
import { resourcePath } from "@/lib/resource-link";
import { seasonVar } from "@/lib/season";
import { threadDisplay, threadHref } from "@/lib/thread-link";
import { Skeleton } from "@/components/ui/skeleton";

/// ホームの「続きから」（最近の会話）と「最近の共有」（自分に共有されたもの）を左右に並べる（#964）。
/// 見出しと区切りはショートカットのカテゴリと同じ（小さな見出し・縦の破線）。
/// 片方が空なら残った方だけを出し、両方とも空（使い始め）なら段ごと出さない。
export function ContinueSection({
  threads,
  shares,
}: {
  threads: Thread[] | null;
  shares: NodeResponse[] | null;
}) {
  // 相対時刻の基準。描画のたびに変えない（一覧が揺れない）。
  const [now] = React.useState(() => Date.now());
  const loading = threads === null || shares === null;
  const showThreads = loading || threads.length > 0;
  const showShares = loading || shares.length > 0;
  if (!showThreads && !showShares) return null;

  return (
    <div className="flex w-full max-w-[700px] flex-wrap gap-y-6">
      {showThreads ? (
        <Column title="続きから">
          {threads === null ? (
            <RowsSkeleton />
          ) : (
            threads.map((t, i) => {
              const d = threadDisplay(t);
              return (
                <Row
                  key={t.id}
                  href={threadHref(t)}
                  icon={d.kind === "note" ? NotebookPen : d.kind === "office" ? FileText : t.agentMode ? Bot : MessageSquareText}
                  seasonIndex={i}
                  title={d.title || "無題の会話"}
                  meta={`${d.kind === null ? (t.agentMode ? "エージェント" : "チャット") : (d.detail ?? "文書の会話")}・${relativeTime(t.updatedAt, now)}`}
                />
              );
            })
          )}
        </Column>
      ) : null}
      {showShares ? (
        <Column title="最近の共有" divided={showThreads}>
          {shares === null ? (
            <RowsSkeleton />
          ) : (
            shares.map((n, i) => (
              <Row
                key={n.id}
                href={resourcePath(n)}
                icon={nodeIcon(n.name)}
                seasonIndex={i + 2}
                title={n.name}
                meta={[n.updated_by_name, relativeTime(n.updated_at, now)].filter(Boolean).join("・")}
              />
            ))
          )}
        </Column>
      ) : null}
    </div>
  );
}

function nodeIcon(name: string): LucideIcon {
  const lower = name.toLowerCase();
  if (lower.endsWith(".md")) return NotebookPen;
  if (lower.endsWith(".xlsx") || lower.endsWith(".csv")) return FileSpreadsheet;
  if (lower.endsWith(".pptx") || lower.endsWith(".slide")) return Presentation;
  return FileText;
}

function Column({
  title,
  divided = false,
  children,
}: {
  title: string;
  divided?: boolean;
  children: React.ReactNode;
}) {
  const id = React.useId();
  return (
    <section
      aria-labelledby={id}
      className={cn("min-w-0 flex-[1_1_300px] px-5", divided && "shiki-divide-l")}
    >
      <h3
        id={id}
        className="mb-3 text-center text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70"
      >
        {title}
      </h3>
      <div className="flex flex-col gap-0.5">{children}</div>
    </section>
  );
}

function Row({
  href,
  icon: Icon,
  seasonIndex,
  title,
  meta,
}: {
  href: string;
  icon: LucideIcon;
  seasonIndex: number;
  title: string;
  meta: string;
}) {
  return (
    <Link
      href={href}
      style={{ ["--season" as string]: seasonVar(seasonIndex) }}
      className="group flex items-center gap-3 rounded-xl px-2.5 py-2 outline-none transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
    >
      <span className="flex size-[34px] shrink-0 items-center justify-center rounded-xl border border-border bg-card text-foreground/70 transition-colors group-hover:border-[var(--season)]/45 group-hover:text-[var(--season)]">
        <Icon className="size-4" aria-hidden />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-[13px] font-medium text-foreground">{title}</span>
        <span className="truncate text-xs text-muted-foreground">{meta}</span>
      </span>
    </Link>
  );
}

function RowsSkeleton() {
  return (
    <>
      {[0, 1, 2].map((i) => (
        <div key={i} className="flex items-center gap-3 px-2.5 py-2">
          <Skeleton className="size-[34px] rounded-xl" />
          <div className="flex flex-1 flex-col gap-1.5">
            <Skeleton className="h-3.5 w-3/5" />
            <Skeleton className="h-3 w-2/5" />
          </div>
        </div>
      ))}
    </>
  );
}
