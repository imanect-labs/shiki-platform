"use client";

/// 製品ヘルプの出典（`help_search` の結果・`[Hn]`）。
///
/// 社内文書の出典（[`CitationSources`](./citations/citation-sources.tsx)）と同じく、回答の下に
/// 「ヘルプ N ページから M 箇所を引用」の 1 行で畳んで出し、開くとページごとに引用箇所を並べる。
/// 本文の `[Hn]` は小さな番号チップにして、押すとその見出しのヘルプを新しいタブで開く。
/// 番号の台帳は社内文書と別（サーバの `HelpCitation`）なので、文脈も別に持つ。
import * as React from "react";
import { ChevronDown, ExternalLink, LifeBuoy } from "lucide-react";

import type { HelpCitation } from "@/lib/chat-api";
import { citedHelpIds, groupHelpPages, safeHelpUrl, uniqueHelpCitations } from "@/lib/help-citation";
import { cn } from "@/lib/utils";
import { CHIP } from "./citations/citation-marker";

type HelpCitationsValue = {
  citations: readonly HelpCitation[];
  /// 本文で引いた番号（本文に `[Hn]` が 1 つも無ければ空）。
  cited: ReadonlySet<number>;
};

const HelpCitationsContext = React.createContext<HelpCitationsValue>({ citations: [], cited: new Set() });

/// 1 件の回答のヘルプ出典を、本文のチップと回答下の一覧に配る。`text` は回答の本文（どの番号を
/// 引いたかを数える。社内文書の出典と同じく、引かなかった検索結果は一覧の最後に畳む）。
export function HelpCitationsProvider({
  citations,
  text,
  children,
}: {
  citations: readonly HelpCitation[];
  text: string;
  children: React.ReactNode;
}) {
  const value = React.useMemo(
    () => ({ citations: uniqueHelpCitations(citations), cited: citedHelpIds(text) }),
    [citations, text],
  );
  return <HelpCitationsContext.Provider value={value}>{children}</HelpCitationsContext.Provider>;
}

export function useHelpCitations(): readonly HelpCitation[] {
  return React.useContext(HelpCitationsContext).citations;
}

/// 見出しの道筋のうち、ページ名の後ろ（「手順」など）。無ければ `null`。
function sectionOf(c: HelpCitation): string | null {
  const path = c.heading_path.filter((h) => h.trim() && h !== c.title);
  return path.length ? path.join(" › ") : null;
}

/// 本文中の `[Hn]`（`linkifyCitations` が `#help-cite-n` のリンクにしたもの）。
export function HelpCitationMarker({ n, children }: { n: number; children: React.ReactNode }) {
  const citation = useHelpCitations().find((c) => c.cite_id === n);
  const url = citation ? safeHelpUrl(citation.url) : null;
  if (!citation || !url) {
    return <span className={cn(CHIP, "bg-muted text-muted-foreground")}>{children}</span>;
  }
  const section = sectionOf(citation);
  const label = `ヘルプ ${n}: ${citation.title}${section ? ` › ${section}` : ""}（新しいタブで開く）`;
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      aria-label={label}
      title={label}
      className={cn(
        CHIP,
        "bg-muted text-muted-foreground no-underline transition-colors hover:bg-foreground/10 hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      H{n}
    </a>
  );
}

/// 回答下の「ヘルプ N ページから M 箇所を引用」。既定は畳む（引用箇所の全文で本文を押し下げない）。
/// 本文に `[Hn]` が無い回答は、届いた出典を全部「参照」として出す。
export function HelpSources({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const { citations, cited } = React.useContext(HelpCitationsContext);
  const [open, setOpen] = React.useState(defaultOpen);
  const [showUnused, setShowUnused] = React.useState(false);
  const listId = React.useId();
  const hasMarkers = citations.some((c) => cited.has(c.cite_id));
  const used = hasMarkers ? citations.filter((c) => cited.has(c.cite_id)) : citations;
  const unused = hasMarkers ? citations.filter((c) => !cited.has(c.cite_id)) : [];
  const pages = groupHelpPages(used);
  if (pages.length === 0) return null;

  return (
    <div className="mt-3" data-testid="help-sources">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-controls={listId}
        className={cn(
          "inline-flex max-w-full items-center gap-2 rounded-full border border-border bg-card py-1 pl-2 pr-3 text-[12.5px] text-foreground/80 transition-colors",
          "hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        )}
      >
        <LifeBuoy className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
        <span className="truncate">
          {hasMarkers
            ? `ヘルプ ${pages.length} ページから ${used.length} 箇所を引用`
            : `ヘルプ ${pages.length} ページを参照`}
        </span>
        <ChevronDown
          className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open ? (
        <section id={listId} className="mt-2.5 space-y-2" aria-label="ヘルプの出典">
          {pages.map((page) => (
            <div key={page.key} className="rounded-xl border border-border bg-card px-3 py-2.5">
              <div className="flex min-w-0 items-center gap-2">
                <LifeBuoy className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                <span className="truncate text-[13.5px] font-medium">{page.title}</span>
                {page.url ? (
                  <a
                    href={page.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-[12px] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    ヘルプで開く
                    <ExternalLink className="size-3" aria-hidden />
                  </a>
                ) : null}
              </div>
              <ul className="mt-1.5 space-y-0.5">
                {page.passages.map((c) => (
                  <HelpPassageRow key={c.cite_id} citation={c} />
                ))}
              </ul>
            </div>
          ))}
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
                  {unused.map((c) => (
                    <UnusedHelpRow key={c.cite_id} citation={c} />
                  ))}
                </ul>
              ) : null}
            </div>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

/// 回答に使わなかったヘルプの検索結果（題名と見出しだけ・押すとその見出しを開く）。
function UnusedHelpRow({ citation }: { citation: HelpCitation }) {
  const url = safeHelpUrl(citation.url);
  const section = sectionOf(citation);
  const inner = (
    <>
      <LifeBuoy className="size-3.5 shrink-0 opacity-70" aria-hidden />
      <span className="truncate">{citation.title}</span>
      {section ? <span className="truncate text-muted-foreground/70">{section}</span> : null}
    </>
  );
  const cls = "flex w-full min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-left text-[12px] text-muted-foreground";
  return (
    <li>
      {url ? (
        <a
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          className={cn(cls, "transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring")}
        >
          {inner}
        </a>
      ) : (
        <div className={cls}>{inner}</div>
      )}
    </li>
  );
}

function HelpPassageRow({ citation }: { citation: HelpCitation }) {
  const url = safeHelpUrl(citation.url);
  const section = sectionOf(citation);
  const body = (
    <>
      <span className="mt-[3px] flex h-[18px] min-w-[18px] shrink-0 items-center justify-center rounded-[5px] bg-muted px-1 text-[10.5px] font-semibold tabular-nums text-muted-foreground">
        H{citation.cite_id}
      </span>
      <span className="min-w-0 flex-1">
        {section ? <span className="block truncate text-[12px] text-foreground/70">{section}</span> : null}
        <span className="line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">{citation.snippet}</span>
      </span>
    </>
  );
  if (!url) return <li className="flex items-start gap-2 px-1.5 py-1">{body}</li>;
  return (
    <li>
      <a
        href={url}
        target="_blank"
        rel="noopener noreferrer"
        className="flex items-start gap-2 rounded-lg px-1.5 py-1 transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {body}
      </a>
    </li>
  );
}
