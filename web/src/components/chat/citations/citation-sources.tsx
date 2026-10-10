"use client";

/// 回答下の出典表示（issue #505・案 E → A）。
///
/// 既定は「N 件の文書から M 箇所を引用」の 1 行。開くと文書ごとのカードに引用箇所を並べ、
/// 回答で使わなかった検索結果は最後に畳む。行のクリックで出典パネルを開く。
import * as React from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import { cn } from "@/lib/utils";
import { citationLocator, type CitedPassage } from "@/lib/citation";
import { useMessageCitations, useSourcePanel, type MessageCitations } from "./citation-context";
import {
  DocIcon,
  NumberBadge,
  OpenOriginalLink,
  SnippetText,
} from "./citation-parts";

const MAX_STACK = 4;

export function CitationSources({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const message = useMessageCitations();
  const [open, setOpen] = React.useState(defaultOpen);
  if (!message || message.citations.length === 0) return null;
  const { docs, hasMarkers } = message.groups;
  const passageCount = docs.reduce((s, d) => s + d.passages.length, 0);
  const listId = `sources-${message.key}`;

  const summary = hasMarkers
    ? `${docs.length} 件の文書から ${passageCount} 箇所を引用`
    : `${docs.length} 件の文書を参照`;

  return (
    <div className="mt-3" data-testid="citation-sources">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-controls={listId}
        className={cn(
          "inline-flex max-w-full items-center gap-2 rounded-full border border-border bg-card py-1 pl-1.5 pr-3 text-[12.5px] text-foreground/80 transition-colors",
          "hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        )}
      >
        <span className="flex shrink-0 -space-x-1.5">
          {docs.slice(0, MAX_STACK).map((d) => (
            <span
              key={d.nodeId}
              className="flex size-6 items-center justify-center rounded-full border-2 border-card bg-muted"
            >
              <DocIcon meta={message.metas[d.nodeId]} className="size-3.5" />
            </span>
          ))}
        </span>
        <span className="truncate">{summary}</span>
        <ChevronDown
          className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open ? <SourceList id={listId} message={message} /> : null}
    </div>
  );
}

function SourceList({ id, message }: { id: string; message: MessageCitations }) {
  const { docs, unused, colorOf } = message.groups;
  const [showUnused, setShowUnused] = React.useState(false);
  return (
    <div id={id} className="mt-2.5 space-y-2">
      {docs.map((d) => {
        const meta = message.metas[d.nodeId];
        return (
          <div key={d.nodeId} className="rounded-xl border border-border bg-card px-3 py-2.5">
            <div className="flex min-w-0 items-center gap-2">
              <DocIcon meta={meta} />
              <span className="truncate text-[13.5px] font-medium">{message.nameOf(d.nodeId)}</span>
              <OpenOriginalLink nodeId={d.nodeId} meta={meta} className="ml-auto" />
            </div>
            <ul className="mt-1.5 space-y-0.5">
              {d.passages.map((p) => (
                <PassageRow key={p.citation.chunk_id} passage={p} message={message} color={colorOf[d.nodeId]} />
              ))}
            </ul>
          </div>
        );
      })}
      {unused.length > 0 ? (
        <div>
          <button
            type="button"
            onClick={() => setShowUnused((v) => !v)}
            aria-expanded={showUnused}
            className="inline-flex items-center gap-1 rounded-md px-1.5 py-1 text-[12px] text-muted-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <ChevronDown className={cn("size-3.5 transition-transform", !showUnused && "-rotate-90")} aria-hidden />
            検索したが回答に使わなかった {unused.length} 件
          </button>
          {showUnused ? (
            <ul className="mt-1 space-y-0.5 pl-1">
              {unused.map((p) => (
                <UnusedRow key={p.citation.chunk_id} passage={p} message={message} />
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function PassageRow({
  passage,
  message,
  color,
}: {
  passage: CitedPassage;
  message: MessageCitations;
  color: number | undefined;
}) {
  const panel = useSourcePanel();
  const { n, citation } = passage;
  const locator = citationLocator(citation);
  const body = (
    <>
      <NumberBadge n={n} colorIndex={color} className="mt-[3px]" />
      <span className="min-w-0 flex-1">
        {locator ? <span className="block truncate text-[12px] text-foreground/70">{locator}</span> : null}
        <span className="line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">
          <SnippetText text={citation.snippet} claim={message.claims.get(n)} around={24} />
        </span>
      </span>
    </>
  );
  if (!panel) return <li className="flex items-start gap-2 px-1.5 py-1">{body}</li>;
  return (
    <li>
      <button
        type="button"
        onClick={() => panel.open(message, n)}
        className="group flex w-full items-start gap-2 rounded-lg px-1.5 py-1 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {body}
        <ChevronRight
          className="mt-1 size-3.5 shrink-0 text-transparent transition-colors group-hover:text-muted-foreground"
          aria-hidden
        />
      </button>
    </li>
  );
}

function UnusedRow({ passage, message }: { passage: CitedPassage; message: MessageCitations }) {
  const panel = useSourcePanel();
  const { n, citation } = passage;
  const meta = message.metas[citation.node_id];
  const locator = citationLocator(citation);
  const inner = (
    <>
      <DocIcon meta={meta} className="size-3.5 opacity-70" />
      <span className="truncate">{message.nameOf(citation.node_id)}</span>
      {locator ? <span className="truncate text-muted-foreground/70">{locator}</span> : null}
    </>
  );
  return (
    <li>
      {panel ? (
        <button
          type="button"
          onClick={() => panel.open(message, n)}
          className="flex w-full min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-left text-[12px] text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {inner}
        </button>
      ) : (
        <div className="flex min-w-0 items-center gap-2 px-1.5 py-1 text-[12px] text-muted-foreground">{inner}</div>
      )}
    </li>
  );
}
