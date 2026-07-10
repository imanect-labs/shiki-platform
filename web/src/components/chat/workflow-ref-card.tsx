"use client";

/// AI が保存したワークフローの参照カード（Task 10.13・workflow_ref ブロック）。
///
/// 表示するのは保存パイプライン（V1〜V7）を通過して artifact 化された参照のみ
/// （backend の不変条件）。カードから dnd エディタへ引き継ぐ。

import Link from "next/link";
import { ArrowRight, Workflow } from "lucide-react";

import { Badge } from "@/components/ui/badge";

type WorkflowRef = {
  id: string;
  name: string;
  displayName: string | null;
  version: number;
};

/// 参照 JSON を防御的にパースする（形が崩れていたら描画しない）。
export function parseWorkflowRef(raw: unknown): WorkflowRef | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as { id?: unknown; name?: unknown; display_name?: unknown; version?: unknown };
  if (typeof r.id !== "string" || typeof r.name !== "string") return null;
  return {
    id: r.id,
    name: r.name,
    displayName: typeof r.display_name === "string" ? r.display_name : null,
    version: typeof r.version === "number" ? r.version : 1,
  };
}

export function WorkflowRefCard({ raw }: { raw: unknown }) {
  const wf = parseWorkflowRef(raw);
  if (!wf) return null;
  return (
    <div className="my-2 flex items-center gap-3 rounded-xl border bg-card p-3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Workflow className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">
            {wf.displayName || wf.name}
          </span>
          <Badge variant="muted">v{wf.version}</Badge>
        </span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          ワークフローを保存しました。エディタで確認・編集できます。
        </span>
      </span>
      <Link
        href={`/workflows/${wf.id}`}
        className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        エディタで開く
        <ArrowRight className="size-3.5" aria-hidden />
      </Link>
    </div>
  );
}
