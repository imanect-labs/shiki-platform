"use client";

/// 回答下の出典表示（issue #505・案 E → A）。
///
/// 既定は「N 件の文書から M 箇所を引用」の 1 行。開くと出所（文書・Web ページ）ごとのカードに
/// 引用箇所を並べ、回答で使わなかった検索結果は最後に畳む。行のクリックで出典パネルを開く。
import * as React from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import { cn } from "@/lib/utils";
import { citationLocator, type CitedDocument, type CitedPassage } from "@/lib/citation";
import { useMessageCitations, useSourcePanel, type MessageCitations } from "./citation-context";
import {
  NumberBadge,
  OpenSourceLink,
  SnippetText,
  SourceIcon,
} from "./citation-parts";

const MAX_STACK = 4;

export function CitationSources({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const message = useMessageCitations();
  const [open, setOpen] = React.useState(defaultOpen);
  if (!message || message.citations.length === 0) return null;
  const { docs, hasMarkers } = message.groups;
  const passageCount = docs.reduce((s, d) => s + d.passages.length, 0);
  const listId = `sources-${message.key}`;

  const [sources, action] = sourcesSummary(docs, passageCount, hasMarkers);

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
              key={d.key}
              className="flex size-6 items-center justify-center rounded-full border-2 border-card bg-muted"
            >
              <SourceIcon citation={d.passages[0].citation} meta={message.metas[d.nodeId]} className="size-3.5" />
            </span>
          ))}
        </span>
        {/* 狭い幅では出所の側を省略し、「M 箇所を引用」は残す。 */}
        <span className="flex min-w-0">
          <span className="truncate">{sources}</span>
          <span className="shrink-0">{action}</span>
        </span>
        <ChevronDown
          className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open ? <SourceList id={listId} message={message} /> : null}
    </div>
  );
}

/// 畳んだ 1 行の文言（出所の側と「から M 箇所を引用」に分けて返す）。文書と Web ページが
/// 混ざるときは短い言い方で両方を数える。どちらか一方なら従来の言い方。
function sourcesSummary(
  docs: readonly CitedDocument[],
  passages: number,
  hasMarkers: boolean,
): [string, string] {
  const web = docs.filter((d) => d.web).length;
  const files = docs.length - web;
  const sources =
    files > 0 && web > 0
      ? `文書 ${files} 件と Web ${web} 件`
      : web > 0
        ? `${web} 件の Web ページ`
        : `${files} 件の文書`;
  return [sources, hasMarkers ? `から ${passages} 箇所を引用` : "を参照"];
}

function SourceList({ id, message }: { id: string; message: MessageCitations }) {
  const { docs, unused, colorOf } = message.groups;
  const [showUnused, setShowUnused] = React.useState(false);
  return (
    <div id={id} className="mt-2.5 space-y-2">
      {docs.map((d) => {
        const meta = message.metas[d.nodeId];
        const first = d.passages[0].citation;
        return (
          <div key={d.key} className="rounded-xl border border-border bg-card px-3 py-2.5">
            <div className="flex min-w-0 items-center gap-2">
              <SourceIcon citation={first} meta={meta} />
              <span className="truncate text-[13.5px] font-medium">{message.nameOf(first)}</span>
              <OpenSourceLink citation={first} meta={meta} locate={false} className="ml-auto" />
            </div>
            <ul className="mt-1.5 space-y-0.5">
              {d.passages.map((p) => (
                <PassageRow key={p.citation.chunk_id} passage={p} message={message} color={colorOf[d.key]} />
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
          // タッチ端末にはホバーが無いので、押せる行だと分かるよう常に薄く出す。
          className="mt-1 size-3.5 shrink-0 text-transparent transition-colors group-hover:text-muted-foreground pointer-coarse:text-muted-foreground/60"
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
      <SourceIcon citation={citation} meta={meta} className="size-3.5 opacity-70" />
      <span className="truncate">{message.nameOf(citation)}</span>
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
