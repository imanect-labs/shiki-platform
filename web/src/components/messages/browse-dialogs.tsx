"use client";

/// 公開チャンネルを探すダイアログと、ダイレクトメッセージの相手を選ぶダイアログ。

import * as React from "react";
import { Archive, Loader2, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { browseChannels, joinChannel, openDm, type ChannelView } from "@/lib/messaging-api";
import { ChannelTile, UserAvatar } from "./primitives";
import { useUserSearch } from "./user-picker";

export function BrowseChannelsDialog({
  open,
  onOpenChange,
  onOpen,
  onJoined,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// 参加せずに開く（公開チャンネルは読める）。
  onOpen: (c: ChannelView) => void;
  onJoined: (c: ChannelView) => void;
}) {
  const [q, setQ] = React.useState("");
  const [archived, setArchived] = React.useState(false);
  const [items, setItems] = React.useState<ChannelView[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [joining, setJoining] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (!open) return;
    let active = true;
    const t = setTimeout(() => {
      browseChannels(q.trim(), archived)
        .then((r) => {
          if (!active) return;
          setItems(r);
          setError(null);
        })
        .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    }, 200);
    return () => {
      active = false;
      clearTimeout(t);
    };
  }, [open, q, archived]);

  React.useEffect(() => {
    if (open) {
      setQ("");
      setArchived(false);
      setItems(null);
    }
  }, [open]);

  const join = async (c: ChannelView) => {
    setJoining(c.id);
    try {
      const joined = await joinChannel(c.id);
      onJoined(joined);
      onOpenChange(false);
    } catch (e) {
      toast({ title: "参加できませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setJoining(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] sm:max-w-[560px]">
        <DialogHeader>
          <DialogTitle>公開チャンネルを探す</DialogTitle>
          <DialogDescription>公開チャンネルは参加しなくても読めます。参加すると一覧に出て、投稿できます。</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <div className="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-lg border border-input px-2.5 focus-within:border-ring/60">
            <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            <input
              autoFocus
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="名前・トピックで探す"
              aria-label="チャンネルを探す"
              className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-muted-foreground/70"
            />
          </div>
          <label className="flex shrink-0 items-center gap-1.5 text-[12px] text-muted-foreground">
            <Switch checked={archived} onCheckedChange={setArchived} aria-label="アーカイブも含める" />
            アーカイブも
          </label>
        </div>
        <div className="rule-soft max-h-[50dvh] min-h-[200px] overflow-y-auto rounded-xl border border-border/60">
          {error ? (
            <p className="px-4 py-6 text-center text-[12.5px] text-destructive" role="alert">
              {error}
            </p>
          ) : items === null ? (
            <div className="flex items-center justify-center gap-2 py-10 text-[12.5px] text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込んでいます
            </div>
          ) : items.length === 0 ? (
            <p className="px-4 py-10 text-center text-[12.5px] text-muted-foreground">該当する公開チャンネルはありません</p>
          ) : (
            items.map((c, i) => (
              <div key={c.id} className={cn("flex items-center gap-3 px-4 py-2.5", i > 0 && "shiki-dash-top")}>
                <ChannelTile channelId={c.id} kind={c.kind} announcement={c.announcement} size="md" />
                <button
                  type="button"
                  onClick={() => {
                    onOpen(c);
                    onOpenChange(false);
                  }}
                  className="min-w-0 flex-1 rounded text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <span className="flex items-center gap-1.5 text-[13px] font-medium text-foreground">
                    <span className="truncate">{c.name}</span>
                    {c.archived ? (
                      <span className="flex shrink-0 items-center gap-0.5 rounded-full bg-muted px-1.5 text-[10px] text-muted-foreground">
                        <Archive className="size-2.5" aria-hidden />
                        アーカイブ
                      </span>
                    ) : null}
                  </span>
                  <span className="block truncate text-[11.5px] text-muted-foreground">
                    {c.member_count} 人{c.topic ? ` ・ ${c.topic}` : ""}
                  </span>
                </button>
                {c.joined ? (
                  <span className="shrink-0 text-[11.5px] text-muted-foreground">参加中</span>
                ) : c.archived ? null : (
                  <Button size="sm" variant="outline" loading={joining === c.id} onClick={() => void join(c)}>
                    参加する
                  </Button>
                )}
              </div>
            ))
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function NewDmDialog({
  open,
  onOpenChange,
  onOpened,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onOpened: (c: ChannelView) => void;
}) {
  const [q, setQ] = React.useState("");
  const { items, loading, error } = useUserSearch(q, open);
  const [busy, setBusy] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) setQ("");
  }, [open]);

  const start = async (userId: string) => {
    setBusy(userId);
    try {
      onOpened(await openDm(userId));
      onOpenChange(false);
    } catch (e) {
      toast({ title: "開けませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] sm:max-w-[460px]">
        <DialogHeader>
          <DialogTitle>ダイレクトメッセージ</DialogTitle>
          <DialogDescription>1 対 1 で話す相手を選びます。3 人以上で話すときは非公開チャンネルを作ってください。</DialogDescription>
        </DialogHeader>
        <div className="flex h-9 items-center gap-2 rounded-lg border border-input px-2.5 focus-within:border-ring/60">
          <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="名前・メールで探す"
            aria-label="相手を探す"
            className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-muted-foreground/70"
          />
          {loading ? <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-hidden /> : null}
        </div>
        <div className="rule-soft max-h-[50dvh] min-h-[160px] overflow-y-auto rounded-xl border border-border/60">
          {error ? (
            <p className="px-4 py-6 text-center text-[12.5px] text-destructive" role="alert">
              {error}
            </p>
          ) : items.length === 0 && !loading ? (
            <p className="px-4 py-10 text-center text-[12.5px] text-muted-foreground">該当する利用者がいません</p>
          ) : (
            items.map((u, i) => (
              <button
                key={u.id}
                type="button"
                disabled={busy !== null}
                onClick={() => void start(u.id)}
                className={cn(
                  "flex w-full items-center gap-2.5 px-3 py-2 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:opacity-60",
                  i > 0 && "shiki-dash-top",
                )}
              >
                <UserAvatar userId={u.id} name={u.display_name || u.email} size="sm" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium text-foreground">{u.display_name || u.email}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{u.email}</span>
                </span>
                {busy === u.id ? <Loader2 className="size-4 animate-spin text-muted-foreground" aria-hidden /> : null}
              </button>
            ))
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
