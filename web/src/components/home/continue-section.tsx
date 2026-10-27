"use client";

import * as React from "react";
import Link from "next/link";
import {
  FileSpreadsheet,
  Folder,
  FileText,
  MessageSquareText,
  NotebookPen,
  Presentation,
  type LucideIcon,
} from "lucide-react";

import type { Thread } from "@/lib/chat-api";
import type { NodeResponse } from "@/lib/storage";
import { relativeTime } from "@/lib/format";
import { resourcePath } from "@/lib/resource-link";
import { seasonVar } from "@/lib/season";
import { threadDisplay, threadHref } from "@/lib/thread-link";
import { Skeleton } from "@/components/ui/skeleton";

/// ホームの「続きから」（最近の会話）と「最近の共有」（自分に共有されたもの）を左右に並べる（#964）。
/// 見出しと区切りはショートカットのカテゴリと同じ（小さな見出し・縦の破線）。幅が足りず縦に積むときは
/// 区切りを出さない。片方が空なら残った方だけを出し、両方とも空（使い始め）なら何も出さない。
export function ContinueSection({
  threads,
  shares,
}: {
  threads: Thread[] | null;
  shares: NodeResponse[] | null;
}) {
  // 相対時刻の基準。描画のたびに変えない（一覧が揺れない）。
  const [now] = React.useState(() => Date.now());
  // 読み込み中（null）は骨組みを出し、読み終えて空なら列ごと出さない（列ごとに判定する）。
  const showThreads = threads === null || threads.length > 0;
  const showShares = shares === null || shares.length > 0;
  if (!showThreads && !showShares) return null;

  return (
    <div className="@container w-full max-w-[700px]">
      <div className="flex flex-col gap-6 @min-[600px]:flex-row @min-[600px]:gap-0">
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
                    icon={d.kind === "note" ? NotebookPen : d.kind === "office" ? FileText : MessageSquareText}
                    seasonIndex={i}
                    title={d.title || "無題の会話"}
                    meta={`${d.detail ?? "チャット"}・${relativeTime(t.updatedAt, now)}`}
                  />
                );
              })
            )}
          </Column>
        ) : null}
        {showThreads && showShares ? (
          <span aria-hidden className="shiki-divide-l hidden w-[1.5px] shrink-0 @min-[600px]:block" />
        ) : null}
        {showShares ? (
          <Column title="最近の共有">
            {shares === null ? (
              <RowsSkeleton />
            ) : (
              shares.map((n, i) => (
                <Row
                  key={n.id}
                  href={resourcePath(n)}
                  icon={nodeIcon(n)}
                  seasonIndex={i + 2}
                  title={n.name}
                  meta={[n.updated_by_name, relativeTime(n.updated_at, now)]
                    .filter(Boolean)
                    .join("・")}
                />
              ))
            )}
          </Column>
        ) : null}
      </div>
    </div>
  );
}

function nodeIcon(node: Pick<NodeResponse, "kind" | "name">): LucideIcon {
  if (node.kind === "folder") return Folder;
  const lower = node.name.toLowerCase();
  if (lower.endsWith(".md")) return NotebookPen;
  if (lower.endsWith(".xlsx") || lower.endsWith(".csv")) return FileSpreadsheet;
  if (lower.endsWith(".pptx") || lower.endsWith(".slide")) return Presentation;
  return FileText;
}

function Column({ title, children }: { title: string; children: React.ReactNode }) {
  const id = React.useId();
  return (
    <section
      aria-labelledby={id}
      className="min-w-0 flex-1 px-5"
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
