"use client";

/// 発言検索。**検索者が参加しているチャンネルと自分の DM だけ**が対象（サーバの二段 authz・design §4.14）。

import * as React from "react";
import { Loader2, Search, ShieldCheck, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { EmptyState } from "@/components/ui/empty-state";
import { plainText, searchMessages, type SearchHit } from "@/lib/messaging-api";
import { AuthorAvatar, ChannelIcon, useMessagingDisplay } from "./primitives";
import { dateTimeLabel } from "./time";

/// 一致箇所を強調する（検索語は正規表現に渡さず、素直に分割する）。
function Highlighted({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const parts = text.split(query);
  return (
    <>
      {parts.map((p, i) => (
        <React.Fragment key={i}>
          {p}
          {i < parts.length - 1 ? (
            <mark className="rounded bg-[color-mix(in_oklab,var(--season-autumn)_28%,transparent)] px-0.5 text-foreground">
              {query}
            </mark>
          ) : null}
        </React.Fragment>
      ))}
    </>
  );
}

export function SearchView({
  leading,
  onClose,
  onJump,
}: {
  leading?: React.ReactNode;
  onClose: () => void;
  /// 結果を開く（返信ならスレッドの親を開く）。
  onJump: (channelId: string, messageId: string, parentId: string | null) => void;
}) {
  const { names, agentName } = useMessagingDisplay();
  const [query, setQuery] = React.useState("");
  const [hits, setHits] = React.useState<SearchHit[] | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const needle = query.trim();

  React.useEffect(() => {
    if (!needle) {
      setHits(null);
      setError(null);
      return;
    }
    let active = true;
    setLoading(true);
    const t = setTimeout(() => {
      searchMessages(needle)
        .then((r) => {
          if (!active) return;
          setHits(r);
          setError(null);
        })
        .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)))
        .finally(() => active && setLoading(false));
    }, 250);
    return () => {
      active = false;
      clearTimeout(t);
    };
  }, [needle]);

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-background" data-testid="message-search">
      <header className="border-b border-border/70 flex h-[60px] shrink-0 items-center gap-1 pl-2 pr-3 lg:px-5">
        {leading}
        <div className="mx-auto flex w-full max-w-3xl items-center gap-2">
          <div className="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-lg border border-border/70 bg-card px-3 transition-colors focus-within:border-ring/60">
            <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape" && !e.nativeEvent.isComposing) onClose();
              }}
              placeholder="発言を検索…"
              aria-label="発言を検索"
              className="min-w-0 flex-1 bg-transparent text-[13.5px] text-foreground outline-none placeholder:text-muted-foreground/70 focus-visible:ring-0 focus-visible:ring-offset-0"
            />
            {loading ? <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-hidden /> : null}
            {query ? (
              <button
                type="button"
                onClick={() => setQuery("")}
                aria-label="検索語を消す"
                className="flex size-5 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3.5" aria-hidden />
              </button>
            ) : null}
          </div>
          <button
            type="button"
            onClick={onClose}
            className="h-9 shrink-0 rounded-lg px-3 text-[13px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            閉じる
          </button>
        </div>
      </header>

      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-3xl px-4 py-4 sm:px-5">
          <div className="mb-3 flex flex-wrap items-center gap-2">
            <p className="flex-1 text-[12.5px] text-muted-foreground" aria-live="polite">
              {!needle ? "検索語を入力してください" : hits ? `${hits.length} 件の発言` : "検索しています…"}
            </p>
            <span className="flex items-center gap-1.5 rounded-full border border-border/60 bg-card/40 px-2.5 py-1 text-[11px] text-muted-foreground">
              <ShieldCheck className="size-3.5" aria-hidden />
              参加しているチャンネルと自分の DM だけを検索
            </span>
          </div>

          {error ? (
            <p className="rounded-xl border border-dashed border-destructive/50 px-4 py-6 text-center text-[12.5px] text-destructive" role="alert">
              {error}
            </p>
          ) : needle && hits && hits.length === 0 ? (
            <EmptyState
              seasonal
              icon={Search}
              title="一致する発言がありません"
              description="参加していないチャンネルや、自分あてでない DM は検索の対象になりません。"
            />
          ) : hits && hits.length > 0 ? (
            <ul className="overflow-hidden rounded-xl border border-border/60 bg-card/40" aria-label="検索結果">
              {hits.map((hit, i) => {
                const m = hit.message;
                return (
                  <li key={`${hit.channel_id}-${m.id}`}>
                    <button
                      type="button"
                      onClick={() => onJump(hit.channel_id, m.id, m.parent_id ?? null)}
                      className={cn(
                        "flex w-full gap-3 px-4 py-3 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                        i > 0 && "border-t border-border/60",
                      )}
                      data-testid="search-hit"
                    >
                      <AuthorAvatar author={m.author} size="sm" />
                      <span className="min-w-0 flex-1">
                        <span className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5">
                          <ChannelIcon kind={hit.channel_kind} className="size-3 text-muted-foreground" />
                          <span className="truncate text-[12px] font-medium text-foreground/80">{hit.channel_name}</span>
                          {m.parent_id ? (
                            <span className="rounded-full bg-muted px-1.5 py-px text-[10px] leading-4 text-muted-foreground">
                              スレッド返信
                            </span>
                          ) : null}
                          <span className="text-[11px] text-muted-foreground">
                            {m.author.kind === "assistant" ? `${m.author.display_name}（${m.author.owner_name ?? "利用者"} のエージェント）` : m.author.display_name}
                            {" ・ "}
                            {dateTimeLabel(m.created_at)}
                          </span>
                        </span>
                        <span className="mt-0.5 block break-words text-[13px] leading-[1.6] text-foreground/90">
                          <Highlighted text={plainText(m, names, agentName)} query={needle} />
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          ) : null}
        </div>
      </div>
    </div>
  );
}
