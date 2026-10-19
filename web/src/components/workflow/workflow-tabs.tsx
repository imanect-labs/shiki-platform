"use client";

/// スマホのワークフロー画面上部の「フロー｜実行履歴」切替（エディタと実行履歴で共用）。
/// PC はバーの操作（実行履歴アイコン・戻る）で行き来するため出さない。

import Link from "next/link";

import { cn } from "@/lib/utils";

export function WorkflowTabs({
  workflowId,
  current,
  className,
}: {
  workflowId: string;
  current: "flow" | "runs";
  className?: string;
}) {
  const tabs = [
    { key: "flow", label: "フロー", href: `/workflows/${workflowId}` },
    { key: "runs", label: "実行履歴", href: `/workflows/${workflowId}/runs` },
  ] as const;
  return (
    <nav
      aria-label="表示の切替"
      className={cn("flex gap-1 rounded-xl bg-muted p-1 md:hidden", className)}
    >
      {tabs.map((t) => {
        const active = t.key === current;
        return (
          <Link
            key={t.key}
            href={t.href}
            replace
            aria-current={active ? "page" : undefined}
            className={cn(
              "flex h-10 flex-1 items-center justify-center rounded-[9px] text-sm font-medium outline-none transition-colors",
              "focus-visible:ring-2 focus-visible:ring-ring",
              active
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground active:bg-background/60",
            )}
          >
            {t.label}
          </Link>
        );
      })}
    </nav>
  );
}
