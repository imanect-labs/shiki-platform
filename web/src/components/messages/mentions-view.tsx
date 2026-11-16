"use client";

/// 自分あてのメンション（アプリ内通知）。開くと既読にし、該当の発言（返信ならスレッド）へ移る。

import * as React from "react";
import { AtSign, CheckCheck, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { listMentions, readMentions, type MentionNotice } from "@/lib/messaging-api";
import { AuthorAvatar } from "./primitives";
import { dateTimeLabel } from "./time";

export function MentionsView({
  leading,
  onJump,
  onChanged,
}: {
  leading?: React.ReactNode;
  onJump: (channelId: string, messageId: string, parentId: string | null) => void;
  /// 既読にした（ナビのバッジを引き直す）。
  onChanged: () => void;
}) {
  const [items, setItems] = React.useState<MentionNotice[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);

  const load = React.useCallback(() => {
    // 最新の一覧（既読を含む）だけだと、それより古い未読が見えず既読にもできない。未読は別に引いて合わせる。
    Promise.all([listMentions(true), listMentions(false)])
      .then(([unreadOnly, recent]) => {
        const byId = new Map<string, MentionNotice>();
        for (const m of [...recent, ...unreadOnly]) byId.set(m.message_id, m);
        setItems([...byId.values()].sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0)));
        setError(null);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, []);
  React.useEffect(load, [load]);

  const unread = (items ?? []).filter((m) => !m.read).length;

  const open = async (m: MentionNotice) => {
    if (!m.read) {
      setItems((prev) => prev?.map((x) => (x.message_id === m.message_id ? { ...x, read: true } : x)) ?? prev);
      readMentions([m.message_id]).then(onChanged, () => undefined);
    }
    onJump(m.channel_id, m.message_id, m.parent_id ?? null);
  };

  const readAll = async () => {
    setBusy(true);
    try {
      await readMentions([]);
      setItems((prev) => prev?.map((x) => ({ ...x, read: true })) ?? prev);
      onChanged();
    } catch (e) {
      toast({ title: "既読にできませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-background">
      <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2 pl-2 pr-3 lg:px-5">
        {leading}
        <h1 className="flex-1 text-[15px] font-semibold">メンション</h1>
        <Button size="sm" variant="ghost" onClick={() => void readAll()} loading={busy} disabled={unread === 0}>
          <CheckCheck className="size-3.5" aria-hidden />
          すべて既読にする
        </Button>
      </header>
      <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-3xl px-4 py-4 sm:px-5">
          {error ? (
            <p className="py-10 text-center text-[12.5px] text-destructive" role="alert">
              {error}
            </p>
          ) : items === null ? (
            <div className="flex items-center justify-center gap-2 py-10 text-[12.5px] text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込んでいます
            </div>
          ) : items.length === 0 ? (
            <EmptyState seasonal icon={AtSign} title="メンションはありません" description="誰かがあなたやあなたのロールをメンションすると、ここに並びます。" />
          ) : (
            <ul className="overflow-hidden rounded-xl border border-border/60 bg-card/40" aria-label="メンション">
              {items.map((m, i) => (
                <li key={m.message_id}>
                  <button
                    type="button"
                    onClick={() => void open(m)}
                    className={cn(
                      "flex w-full gap-3 px-4 py-3 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                      i > 0 && "shiki-dash-top",
                      !m.read && "bg-accent/40",
                    )}
                  >
                    <AuthorAvatar author={m.author} size="sm" />
                    <span className="min-w-0 flex-1">
                      <span className="flex flex-wrap items-center gap-x-1.5 text-[12px]">
                        {!m.read ? <span className="size-1.5 rounded-full bg-primary" aria-label="未読" /> : null}
                        <span className="font-medium text-foreground/85">{m.author.display_name}</span>
                        <span className="text-muted-foreground">・ {m.channel_name}</span>
                        <span className="text-[11px] text-muted-foreground">・ {dateTimeLabel(m.created_at)}</span>
                      </span>
                      <span className="mt-0.5 line-clamp-2 block text-[13px] leading-[1.6] text-foreground/90">{m.excerpt}</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
