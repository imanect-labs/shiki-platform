"use client";

import { ShieldCheck, Timer } from "lucide-react";

import { cn } from "@/lib/utils";
import type { SearchDebug } from "@/lib/search";

/// 検索パイプラインの段階ファネル可視化（Task 2.10 受入条件:
/// 権限で絞られた件数・どの段で絞られたかのデバッグ表示）。
export function DebugPanel({ debug }: { debug: SearchDebug }) {
  const funnel = [
    { label: "dense 候補", value: debug.dense_hits },
    { label: "keyword 候補", value: debug.keyword_hits },
    { label: "RRF 融合", value: debug.fused },
    {
      label: "権限 deny",
      value: debug.authz_denied_chunks,
      accent: "denied" as const,
    },
    { label: "rerank", value: debug.reranked },
  ];
  const stages = [
    ["可読集合", debug.stage_ms.readable_set_ms],
    ["埋め込み", debug.stage_ms.embed_ms],
    ["検索", debug.stage_ms.retrieve_ms],
    ["認可検証", debug.stage_ms.post_filter_ms],
    ["rerank", debug.stage_ms.rerank_ms],
    ["本文取得", debug.stage_ms.hydrate_ms],
  ] as const;
  const totalMs = stages.reduce((acc, [, ms]) => acc + ms, 0);

  return (
    <section
      aria-label="検索デバッグ情報"
      className="rounded-xl border border-border bg-muted/20 p-4 text-xs"
    >
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <span className="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 font-medium text-primary">
          <ShieldCheck className="size-3.5" aria-hidden />
          pre-filter: {debug.prefilter_mode === "tags" ? "可読タグ" : "テナントのみ（縮退）"}
        </span>
        <span className="text-muted-foreground">可読タグ {debug.readable_tags} 件</span>
        {debug.backfill_rounds > 1 ? (
          <span className="text-muted-foreground">
            バックフィル {debug.backfill_rounds - 1} 回
          </span>
        ) : null}
        <span className="ml-auto inline-flex items-center gap-1 tabular-nums text-muted-foreground">
          <Timer className="size-3.5" aria-hidden />
          {totalMs}ms
        </span>
      </div>

      {/* 段階ファネル: 各段で何件に絞られたか */}
      <ol className="flex flex-wrap items-center gap-1.5" aria-label="絞り込みファネル">
        {funnel.map((step, i) => (
          <li key={step.label} className="flex items-center gap-1.5">
            {i > 0 ? <span className="text-muted-foreground/50">→</span> : null}
            <span
              className={cn(
                "inline-flex items-baseline gap-1 rounded-md border px-2 py-1",
                step.accent === "denied"
                  ? "border-destructive/30 bg-destructive/5 text-destructive"
                  : "border-border bg-background text-foreground",
              )}
            >
              <span className="font-semibold tabular-nums">{step.value}</span>
              <span
                className={cn(
                  "text-[10px]",
                  step.accent === "denied" ? "text-destructive/80" : "text-muted-foreground",
                )}
              >
                {step.label}
              </span>
            </span>
          </li>
        ))}
      </ol>
      {debug.authz_denied_files > 0 ? (
        <p className="mt-2 text-destructive/90">
          権限フィルタで {debug.authz_denied_files} ファイル（{debug.authz_denied_chunks}{" "}
          チャンク）が除外されました。
        </p>
      ) : null}

      {/* 段別レイテンシ */}
      <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground">
        {stages.map(([label, ms]) => (
          <span key={label} className="tabular-nums">
            {label} {ms}ms
          </span>
        ))}
      </div>
    </section>
  );
}
