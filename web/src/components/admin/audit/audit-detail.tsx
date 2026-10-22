"use client";

import * as React from "react";
import Link from "next/link";
import { Check, ExternalLink, Filter, Link2, X } from "lucide-react";

import type { AuditEvent, AuditQuery } from "@/lib/audit-api";
import { Badge } from "@/components/ui/badge";
import { citedChunks, formatTime, objectHref } from "./audit-query";

export function Verdict({ decision }: { decision: string }) {
  const allow = decision === "allow";
  return (
    <Badge variant={allow ? "success" : "destructive"} className="gap-1">
      {allow ? <Check className="size-3" aria-hidden /> : <X className="size-3" aria-hidden />}
      {allow ? "許可" : "拒否"}
    </Badge>
  );
}

/// 操作した人の表示（名簿の表示名、無ければ ID）。
export function actorLabel(e: AuditEvent): string {
  return e.actor_name || e.actor;
}

/// 「この値で絞る」の小さなボタン。
function NarrowButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="inline-flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-9"
    >
      <Filter className="size-3.5" aria-hidden />
    </button>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="pt-0.5 text-muted-foreground">{label}</dt>
      <dd className="flex min-w-0 items-start gap-1">{children}</dd>
    </>
  );
}

/// 1 行の詳細。値から「この値で絞る」ができる（同じ人・同じ対象・同じ要求の記録をたどる）。
/// ファイル名や本文は出さない。開くリンクは通常の権限で判定される（PIT-65）。
export function AuditDetail({
  event: e,
  onNarrow,
}: {
  event: AuditEvent;
  onNarrow: (p: Partial<AuditQuery>) => void;
}) {
  const chunks = citedChunks(e.metadata);
  const href = objectHref(e.object_type, e.object_id);
  const metadata = JSON.stringify(e.metadata, null, 2);
  return (
    <div className="flex flex-col gap-3">
      <dl className="grid grid-cols-[76px_minmax(0,1fr)] gap-x-3 gap-y-2 text-[12.5px] max-md:text-sm">
        <Row label="時刻">
          <span className="font-mono tabular-nums">{formatTime(e.created_at)}</span>
        </Row>
        <Row label="判定">
          <Verdict decision={e.decision} />
        </Row>
        <Row label="誰が">
          <span className="min-w-0 flex-1 break-all">
            {e.actor_name ? (
              <>
                {e.actor_name}
                <span className="block font-mono text-[11.5px] text-muted-foreground">{e.actor}</span>
              </>
            ) : (
              <span className="font-mono">{e.actor}</span>
            )}
          </span>
          <NarrowButton label="この人の記録で絞る" onClick={() => onNarrow({ actor: e.actor })} />
        </Row>
        <Row label="操作">
          <span className="min-w-0 flex-1 break-all font-mono">{e.action}</span>
          <NarrowButton label="この操作で絞る" onClick={() => onNarrow({ action: e.action })} />
        </Row>
        <Row label="対象">
          <span className="min-w-0 flex-1 break-all font-mono">
            <span className="text-muted-foreground">{e.object_type}:</span>
            {e.object_id}
          </span>
          <NarrowButton
            label="この対象の記録で絞る"
            onClick={() => onNarrow({ object_type: e.object_type, object_id: e.object_id })}
          />
        </Row>
        {e.trace_id ? (
          <Row label="trace_id">
            <span className="min-w-0 flex-1 break-all font-mono">{e.trace_id}</span>
            <NarrowButton
              label="同じ要求の記録で絞る"
              onClick={() => onNarrow({ trace_id: e.trace_id ?? undefined })}
            />
          </Row>
        ) : null}
        <Row label="改ざん検知">
          <span className="text-muted-foreground">
            {e.chained ? "ハッシュチェーンに連結" : "対象外（読み取り・拒否の記録）"}
          </span>
        </Row>
      </dl>
      {href ? (
        <Link
          href={href}
          target="_blank"
          rel="noreferrer"
          className="inline-flex w-fit items-center gap-1.5 rounded-md text-[12.5px] font-medium underline-offset-4 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
        >
          <ExternalLink className="size-3.5" aria-hidden />
          対象を開く（閲覧権限があるときだけ開けます）
        </Link>
      ) : null}
      {chunks.length > 0 ? (
        <section aria-label="引用されたチャンク" className="flex flex-col gap-1.5">
          <h3 className="text-xs text-muted-foreground">引用されたチャンク（{chunks.length}）</h3>
          <ul className="flex flex-col gap-1">
            {chunks.map((c) => (
              <li key={c} className="flex items-center gap-1 text-[12px]">
                <Link2 className="size-3 shrink-0 text-muted-foreground" aria-hidden />
                <span className="min-w-0 flex-1 truncate font-mono" title={c}>
                  {c}
                </span>
                <NarrowButton label="このチャンクを引用した記録で絞る" onClick={() => onNarrow({ chunk_id: c })} />
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      <section aria-label="付帯情報" className="flex flex-col gap-1.5">
        <h3 className="text-xs text-muted-foreground">付帯情報（metadata）</h3>
        <pre className="max-h-72 overflow-auto rounded-md bg-muted/50 p-2.5 font-mono text-[11.5px] leading-relaxed">
          {metadata === "{}" ? "（なし）" : metadata}
        </pre>
      </section>
    </div>
  );
}
