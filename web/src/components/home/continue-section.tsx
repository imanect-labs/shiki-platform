"use client";

import * as React from "react";
import Link from "next/link";
import {
  CircleAlert,
  FileSpreadsheet,
  Folder,
  FileText,
  Gauge,
  MessageSquareText,
  NotebookPen,
  Presentation,
  ShieldCheck,
  Workflow,
  type LucideIcon,
} from "lucide-react";

import type { HomeFeed } from "@/hooks/use-home-feed";
import { usagePercent } from "@/lib/home-today";
import type { NodeResponse } from "@/lib/storage";
import { formatUsd } from "@/components/admin/format";
import { cn } from "@/lib/utils";
import { relativeTime } from "@/lib/format";
import { resourcePath } from "@/lib/resource-link";
import { seasonVar } from "@/lib/season";
import { threadDisplay, threadHref } from "@/lib/thread-link";
import { Skeleton } from "@/components/ui/skeleton";

/// 「今日のこと」の列に出す行数の上限（承認待ち → 失敗 → 集計 → 利用量 → 最近の共有の順に詰める）。
const TODAY_MAX_ROWS = 5;

/// 「今日のこと」の 1 行。`urgent` は対応が要るもの（承認待ち・ワークフローの失敗）で、スマホでも出す。
type TodayItem = {
  key: string;
  href?: string;
  icon: LucideIcon;
  title: string;
  meta: string;
  urgent: boolean;
  tone?: "danger" | "warn";
  /// 0〜100。指定すると行の右に小さな進み具合を出す（今月の利用）。
  progress?: number;
};

/// フィードから「今日のこと」の行を組み立てる（対応が要るものを先に）。
function todayItems(feed: HomeFeed, now: number): TodayItem[] {
  const items: TodayItem[] = [];
  for (const a of (feed.approvals ?? []).slice(0, 2)) {
    // 文書の横で始めた会話は文書の画面で開く（「続きから」と同じ解決・タイトルの接頭も外す）。
    const thread = {
      id: a.thread_id,
      title: a.thread_title,
      originNoteId: a.origin_note_id ?? null,
      originNoteName: a.origin_note_name ?? null,
    };
    items.push({
      key: `approval:${a.run_id}`,
      href: threadHref(thread),
      icon: ShieldCheck,
      title: threadDisplay(thread).title || "無題の会話",
      // ツールの入力は持たない（機微・大きい）ので、何をするかは会話の承認カードで確かめてもらう。
      meta: `承認待ち・${relativeTime(a.requested_at, now)}`,
      urgent: true,
    });
  }
  const wf = feed.workflows;
  for (const f of (wf?.recent_failures ?? []).slice(0, 2)) {
    items.push({
      key: `failure:${f.run_id}`,
      href: `/workflows/${f.workflow_id}/runs?run=${f.run_id}`,
      icon: CircleAlert,
      title: f.display_name || f.workflow_name,
      meta: `ワークフローが失敗・${relativeTime(f.finished_at, now)}`,
      urgent: true,
      tone: "danger",
    });
  }
  if (wf && wf.failed === 0 && wf.succeeded > 0) {
    items.push({
      key: "workflows",
      href: "/workflows",
      icon: Workflow,
      title: "ワークフロー",
      meta: `直近 ${wf.window_hours} 時間で ${wf.succeeded} 件成功`,
      urgent: false,
    });
  }
  const usage = feed.usage;
  const pct = usage ? usagePercent(usage) : null;
  if (usage && pct !== null) {
    items.push({
      key: "usage",
      icon: Gauge,
      title: "今月の AI 利用",
      meta: `上限の ${pct}%（${formatUsd(usage.used_usd_micros ?? 0)} / ${formatUsd(usage.limit_usd_micros ?? 0)}）`,
      urgent: false,
      tone: pct >= 80 ? "warn" : undefined,
      progress: pct,
    });
  }
  for (const n of feed.shares ?? []) {
    if (items.length >= TODAY_MAX_ROWS) break;
    items.push({
      key: `share:${n.id}`,
      href: resourcePath(n),
      icon: nodeIcon(n),
      title: n.name,
      meta: ["共有", n.updated_by_name, relativeTime(n.updated_at, now)].filter(Boolean).join("・"),
      urgent: false,
    });
  }
  return items.slice(0, TODAY_MAX_ROWS);
}

/// ホームの「続きから」（最近の会話）と「今日のこと」（承認待ち・ワークフローの失敗・今月の利用・
/// 最近の共有）を左右に並べる（#964・#965）。見出しと区切りはショートカットのカテゴリと同じ
/// （小さな見出し・縦の破線）。幅が足りず縦に積むときは区切りを出さない。片方が空なら残った方だけを
/// 出し、両方とも空（使い始め）なら何も出さない。
/// スマホは「続きから」の 3 件と、対応が要るもの（承認待ち・失敗）だけにする（縦に長くしない）。
export function ContinueSection({ feed }: { feed: HomeFeed }) {
  const { threads } = feed;
  // 相対時刻の基準。描画のたびに変えない（一覧が揺れない）。
  const [now] = React.useState(() => Date.now());
  // 「今日のこと」は読めた分から出す（遅い取得を待たない）。何も読めていない間だけ骨組みを出し、
  // 全部読み終えて空なら列ごと出さない（列ごとに判定する）。
  const today = todayItems(feed, now);
  const todayDone = feed.shares !== null && feed.todayLoaded;
  const todayLoading = today.length === 0 && !todayDone;
  const showThreads = threads === null || threads.length > 0;
  const showToday = todayLoading || today.length > 0;
  if (!showThreads && !showToday) return null;
  const urgent = today.some((t) => t.urgent);

  return (
    // スマホで出すものが無ければ段ごと出さない（会話が無く、対応の要るものも無い）。
    <div
      className={cn(
        "@container w-full max-w-[700px]",
        threads?.length === 0 && !urgent && "max-sm:hidden",
      )}
    >
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
                    className={i >= 3 ? "max-sm:hidden" : undefined}
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
        {showThreads && showToday ? (
          <span aria-hidden className="shiki-divide-l hidden w-[1.5px] shrink-0 @min-[600px]:block" />
        ) : null}
        {showToday ? (
          <Column title="今日のこと" className={urgent ? undefined : "max-sm:hidden"}>
            {todayLoading ? (
              <RowsSkeleton />
            ) : (
              today.map((t, i) => (
                <Row
                  key={t.key}
                  className={t.urgent ? undefined : "max-sm:hidden"}
                  href={t.href}
                  icon={t.icon}
                  seasonIndex={i + 2}
                  title={t.title}
                  meta={t.meta}
                  tone={t.tone}
                  progress={t.progress}
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

function Column({
  title,
  className,
  children,
}: {
  title: string;
  className?: string;
  children: React.ReactNode;
}) {
  const id = React.useId();
  return (
    <section
      aria-labelledby={id}
      className={cn("min-w-0 flex-1 px-5", className)}
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
  className,
  href,
  icon: Icon,
  seasonIndex,
  title,
  meta,
  tone,
  progress,
}: {
  className?: string;
  href?: string;
  icon: LucideIcon;
  seasonIndex: number;
  title: string;
  meta: string;
  tone?: "danger" | "warn";
  progress?: number;
}) {
  const body = (
    <>
      <span
        className={cn(
          "flex size-[34px] shrink-0 items-center justify-center rounded-xl border border-border bg-card text-foreground/70 transition-colors",
          href && "group-hover:border-[var(--season)]/45",
          // 状態の色（失敗・注意）はホバーでも季節の色に上書きしない。
          tone === "danger"
            ? "text-destructive"
            : tone === "warn"
              ? "text-[var(--season-autumn)]"
              : href && "group-hover:text-[var(--season)]",
        )}
      >
        <Icon className="size-4" aria-hidden />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-[13px] font-medium text-foreground">{title}</span>
        <span className="truncate text-xs text-muted-foreground">{meta}</span>
      </span>
      {progress !== undefined ? (
        <span aria-hidden className="h-1.5 w-14 shrink-0 overflow-hidden rounded-full bg-muted">
          <span
            className={cn(
              "block h-full rounded-full",
              tone === "warn" ? "bg-[var(--season-autumn)]" : "bg-[var(--season-winter)]",
            )}
            style={{ width: `${progress}%` }}
          />
        </span>
      ) : null}
    </>
  );
  const rowClass = cn("flex items-center gap-3 rounded-xl px-2.5 py-2", className);
  if (!href) return <div className={rowClass}>{body}</div>;
  return (
    <Link
      href={href}
      style={{ ["--season" as string]: seasonVar(seasonIndex) }}
      className={cn(
        rowClass,
        "group outline-none transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      {body}
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
