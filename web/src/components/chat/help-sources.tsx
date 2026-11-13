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
import { groupHelpPages, safeHelpUrl, uniqueHelpCitations } from "@/lib/help-citation";
import { cn } from "@/lib/utils";
import { CHIP } from "./citations/citation-marker";

const HelpCitationsContext = React.createContext<readonly HelpCitation[]>([]);

/// 1 件の回答のヘルプ出典を、本文のチップと回答下の一覧に配る。
export function HelpCitationsProvider({
  citations,
  children,
}: {
  citations: readonly HelpCitation[];
  children: React.ReactNode;
}) {
  const unique = React.useMemo(() => uniqueHelpCitations(citations), [citations]);
  return <HelpCitationsContext.Provider value={unique}>{children}</HelpCitationsContext.Provider>;
}

export function useHelpCitations(): readonly HelpCitation[] {
  return React.useContext(HelpCitationsContext);
}

/// 見出しの道筋のうち、ページ名の後ろ（「手順」など）。無ければ `null`。
function sectionOf(c: HelpCitation): string | null {
  const path = c.heading_path.filter((h) => h.trim() && h !== c.title);
  return path.length ? path.join(" › ") : null;
}

/// 本文中の `[Hn]`（`linkifyHelpCitations` が `#help-cite-n` のリンクにしたもの）。
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
export function HelpSources({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const citations = useHelpCitations();
  const [open, setOpen] = React.useState(defaultOpen);
  const listId = React.useId();
  const pages = React.useMemo(() => groupHelpPages(citations), [citations]);
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
          ヘルプ {pages.length} ページから {citations.length} 箇所を引用
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
        </section>
      ) : null}
    </div>
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
