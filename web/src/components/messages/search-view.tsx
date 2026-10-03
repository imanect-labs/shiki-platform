"use client";

/// 発言検索。**検索者が参加しているチャンネルと自分あての DM だけ**を対象にする（FR-18）。
/// 実装では RAG と同型の二段 authz（pre-filter ＝ 参加チャンネルへの絞り込み／
/// post-filter ＝ 結果の channel 再評価）になる。ここは pre-filter 相当だけを模す。

import * as React from "react";
import { Search, ShieldCheck, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { EmptyState } from "@/components/ui/empty-state";
import {
  channelTitle,
  findMember,
  plainText,
  searchMessages,
  type Channel,
} from "@/lib/messages-mock";
import { ChannelIcon, MemberAvatar } from "./primitives";

/// 一致箇所を太字にする（検索語は正規表現に渡さず、素直に分割する）。
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
  channels,
  viewerId,
  onClose,
  onJump,
}: {
  channels: Channel[];
  viewerId: string;
  onClose: () => void;
  onJump: (channelId: string, messageId: string) => void;
}) {
  const [query, setQuery] = React.useState("予算");
  // 照合は trim 済みの語で行う（Highlighted も同じ語で切らないとハイライトが消える）。
  const needle = query.trim();
  const hits = React.useMemo(
    () => searchMessages(channels, query, viewerId),
    [channels, query, viewerId],
  );
  // 参加チャンネル数だけを出す。**非参加チャンネルの件数は出さない** —
  // 非公開チャンネルと他人の DM は非メンバーには存在ごと見えないため、
  // 「N 件は対象外」は隠すべきものの件数を漏らす。
  const mine = channels.filter((c) => c.memberIds.includes(viewerId)).length;

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-background">
      <header className="shiki-dash-bottom flex h-14 shrink-0 items-center px-5">
        <div className="mx-auto flex w-full max-w-3xl items-center gap-2">
        <div className="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-lg border border-border/70 bg-card px-3 transition-colors focus-within:border-ring/60">
          <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="発言を検索…"
            aria-label="発言を検索"
            className="min-w-0 flex-1 bg-transparent text-[13.5px] text-foreground outline-none placeholder:text-muted-foreground/70 focus-visible:ring-0 focus-visible:ring-offset-0"
          />
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
        <div className="mx-auto w-full max-w-3xl px-5 py-4">
          <div className="mb-3 flex items-center gap-2">
            <p className="flex-1 text-[12.5px] text-muted-foreground">
              {query ? `${hits.length} 件の発言` : "検索語を入力してください"}
            </p>
            <span className="flex items-center gap-1.5 rounded-full border border-border/60 bg-card/40 px-2.5 py-1 text-[11px] text-muted-foreground">
              <ShieldCheck className="size-3.5" aria-hidden />
              {findMember(viewerId).name} が参加する {mine} チャンネルを検索
            </span>
          </div>

          {query && hits.length === 0 ? (
            <EmptyState
              seasonal
              icon={Search}
              title="一致する発言がありません"
              description="参加していないチャンネルや、自分あてでない DM は検索の対象になりません。"
            />
          ) : (
            <ul className="rule-soft overflow-hidden rounded-xl border border-border/60 bg-card/40">
              {hits.map((hit, i) => {
                const author = findMember(hit.message.authorId);
                return (
                  <li key={`${hit.channel.id}-${hit.message.id}`}>
                    <button
                      type="button"
                      onClick={() => onJump(hit.channel.id, hit.parent?.id ?? hit.message.id)}
                      className={cn(
                        "flex w-full gap-3 px-4 py-3 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring",
                        i > 0 && "shiki-dash-top",
                      )}
                    >
                      <MemberAvatar memberId={hit.message.authorId} size="sm" />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-1.5">
                          <ChannelIcon
                            kind={hit.channel.kind}
                            memberCount={hit.channel.memberIds.length}
                            className="size-3 text-muted-foreground"
                          />
                          <span className="truncate text-[12px] font-medium text-foreground/80">
                            {channelTitle(hit.channel, viewerId)}
                          </span>
                          {hit.parent ? (
                            <span className="rounded-full bg-muted px-1.5 py-px text-[10px] leading-4 text-muted-foreground">
                              スレッド返信
                            </span>
                          ) : null}
                          <span className="text-[11px] text-muted-foreground">
                            {author.name} ・ {hit.message.dayLabel} {hit.message.time}
                          </span>
                        </span>
                        <span className="mt-0.5 block text-[13px] leading-[1.6] text-foreground/90">
                          <Highlighted text={plainText(hit.message, viewerId)} query={needle} />
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}

          <p className="mt-3 px-1 text-[11.5px] leading-relaxed text-muted-foreground/80">
            発言は RAG の索引には入れず、専用の全文検索に持ちます。実装では、参加チャンネルへの
            絞り込み（pre-filter）と結果ごとの再評価（post-filter）の二段で守ります。
            この画面はモックのため、絞り込みの側だけを再現しています。
          </p>
        </div>
      </div>
    </div>
  );
}
