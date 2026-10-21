"use client";

/// 根拠を確認（issue #505・案 F）。回答の各文と、その文が引用した原文を対照表で並べる。
/// 常時出すと重いので、メッセージのアクションから開く。
import * as React from "react";
import { ListChecks } from "lucide-react";

import { cn } from "@/lib/utils";
import { FooterButton } from "@/components/chat/message-footer";
import { citationAt, citationLocator } from "@/lib/citation";
import { useMessageCitations, useSourcePanel } from "./citation-context";
import { DocIcon, NumberBadge, SnippetText } from "./citation-parts";

export function EvidenceTable() {
  const message = useMessageCitations();
  const panel = useSourcePanel();
  if (!message) return null;
  const runs = message.runs.filter((r) => r.claim);
  if (runs.length === 0) return null;
  const { colorOf } = message.groups;

  return (
    <div className="mt-3 overflow-hidden rounded-xl border border-border" data-testid="evidence-table">
      <div className="flex flex-wrap items-baseline gap-x-2 border-b border-border bg-muted/40 px-3 py-2 text-[12px]">
        <span className="font-medium text-foreground/80">根拠を確認</span>
        <span className="text-muted-foreground">回答の各文と、引用した原文の対応</span>
      </div>
      <div className="divide-y divide-border/70">
        {runs.map((r, i) => (
          <div
            key={i}
            className="grid grid-cols-1 gap-2 px-3 py-2.5 sm:grid-cols-[minmax(0,0.85fr)_minmax(0,1.3fr)] sm:gap-3"
          >
            <div className="flex items-start gap-2 text-[12.5px] leading-relaxed text-foreground/85">
              <span className="mt-[2px] flex shrink-0 gap-0.5">
                {r.ns.map((n) => {
                  const c = citationAt(message.citations, n)!;
                  return <NumberBadge key={n} n={n} colorIndex={colorOf[c.node_id]} />;
                })}
              </span>
              <span>{r.claim}</span>
            </div>
            <div className="min-w-0 space-y-2">
              {r.ns.map((n) => {
                const c = citationAt(message.citations, n)!;
                const meta = message.metas[c.node_id];
                const locator = citationLocator(c);
                const Body = panel ? "button" : "div";
                return (
                  <Body
                    key={n}
                    {...(panel ? { type: "button" as const, onClick: () => panel.open(message, n) } : {})}
                    className={cn(
                      "block w-full rounded-lg text-left",
                      panel &&
                        "-mx-1.5 px-1.5 py-1 transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    )}
                  >
                    <span className="line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">
                      <SnippetText text={c.snippet} claim={r.claim} around={16} />
                    </span>
                    <span className="mt-1 flex min-w-0 items-center gap-1.5 text-[11px] text-muted-foreground">
                      <DocIcon meta={meta} className="size-3" />
                      <span className="truncate">{message.nameOf(c.node_id)}</span>
                      {locator ? <span className="shrink-0">・ {locator}</span> : null}
                    </span>
                  </Body>
                );
              })}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/// メッセージのアクション行に置く「根拠を確認」トグル。本文に引用が無ければ出さない。
export function EvidenceToggle({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  const message = useMessageCitations();
  if (!message || !message.runs.some((r) => r.claim)) return null;
  return (
    <FooterButton label="根拠を確認" onClick={onToggle} pressed={open}>
      <ListChecks className="size-3.5" />
    </FooterButton>
  );
}
