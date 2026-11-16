"use client";

/// 共有ダイアログの「チャンネル」部分（ドライブのファイル・フォルダだけ・design §4.14）。
///
/// - 自分が参加しているチャンネルへ**閲覧で**共有する（編集は付けられない）。参加者全員が読めるようになり、
///   メッセージのエージェントの公開返信の根拠にも使われる。
/// - 共有中のチャンネルは名前で出し、行ごとに解除できる。名前を読めないチャンネル（退出したなど）は
///   「参照できないチャンネル」と出す。
/// - 共有・解除の認可（owner かつ参加者）はサーバが決める。ここは候補を絞るだけ。

import * as React from "react";
import { Hash, Loader2, Lock, MessageSquare, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { listChannels, type ChannelKind, type ChannelView } from "@/lib/messaging-api";
import {
  listChannelShares,
  shareWithChannel,
  unshareFromChannel,
  type ChannelShareEntry,
} from "@/lib/storage";

export function ShareChannelsPanel({ open, nodeId }: { open: boolean; nodeId: string }) {
  const [channels, setChannels] = React.useState<ChannelView[]>([]);
  const [shares, setShares] = React.useState<ChannelShareEntry[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<string | null>(null);
  const [pending, setPending] = React.useState<string | null>(null);

  const refresh = React.useCallback(async () => {
    const [mine, current] = await Promise.all([listChannels(), listChannelShares(nodeId)]);
    // チャンネル（公開・非公開）を先に、DM を後に並べる（名前順）。
    const rank = (c: ChannelView) => (c.kind === "dm" ? 1 : 0);
    setChannels(
      mine
        .filter((c) => !c.archived)
        .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name, "ja")),
    );
    setShares(current);
  }, [nodeId]);

  React.useEffect(() => {
    if (!open) return;
    let active = true;
    setLoading(true);
    setError(null);
    refresh()
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [open, refresh]);

  const shared = new Set(shares.map((s) => s.channel_id));

  const run = async (channelId: string, action: () => Promise<void>, failure: string) => {
    setPending(channelId);
    try {
      await action();
      await refresh();
    } catch (e) {
      toast({
        variant: "destructive",
        title: failure,
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setPending(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込み中…
      </div>
    );
  }
  if (error) {
    return (
      <div className="py-6 text-center text-sm">
        <p className="text-destructive">チャンネルを読み込めませんでした。</p>
        <button
          type="button"
          onClick={() => {
            setLoading(true);
            setError(null);
            refresh()
              .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
              .finally(() => setLoading(false));
          }}
          className="mt-2 text-muted-foreground underline underline-offset-2 hover:text-foreground"
        >
          再試行
        </button>
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <p className="text-xs leading-relaxed text-muted-foreground">
        チャンネルの参加者全員が<strong className="font-medium text-foreground">閲覧</strong>
        できるようになります。エージェントがチャンネルに公開で答えるときの根拠にも使われます。
      </p>

      <div className="rounded-lg border border-border">
        {channels.length === 0 ? (
          <p className="px-3 py-5 text-center text-sm text-muted-foreground">参加しているチャンネルがありません</p>
        ) : (
          <ul className="max-h-64 divide-y divide-border overflow-y-auto">
            {channels.map((c) => {
              const already = shared.has(c.id);
              return (
                <li key={c.id} data-testid="share-channel-candidate" className="flex items-center gap-2.5 px-3 py-2">
                  <ChannelAvatar kind={c.kind} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{c.name}</p>
                    <p className="truncate text-xs text-muted-foreground">
                      {kindLabel(c.kind)} · 参加者 {c.member_count} 人
                    </p>
                  </div>
                  <Button
                    type="button"
                    size="sm"
                    variant={already ? "ghost" : "outline"}
                    disabled={already || pending === c.id}
                    aria-label={already ? `${c.name} に共有済み` : `${c.name} に共有`}
                    onClick={() =>
                      void run(c.id, () => shareWithChannel(nodeId, c.id), "チャンネルに共有できませんでした")
                    }
                    className="shrink-0 pointer-coarse:h-9"
                  >
                    {pending === c.id ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                    {already ? "共有済み" : "共有"}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="flex flex-col gap-1.5">
        <p className="text-sm font-medium">共有中のチャンネル</p>
        {shares.length === 0 ? (
          <p className="py-1 text-sm text-muted-foreground">まだどのチャンネルにも共有していません。</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border rounded-lg border border-border">
            {shares.map((s) => {
              const name = s.name ?? "参照できないチャンネル";
              return (
                <li
                  key={s.channel_id}
                  data-testid="share-channel-entry"
                  className="flex items-center gap-2.5 py-1.5 pl-3 pr-1"
                >
                  <ChannelAvatar kind={s.kind ?? null} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">{name}</p>
                    <p className="truncate text-xs text-muted-foreground">閲覧</p>
                  </div>
                  <button
                    type="button"
                    aria-label={`${name} への共有を解除`}
                    disabled={pending === s.channel_id}
                    onClick={() =>
                      void run(
                        s.channel_id,
                        () => unshareFromChannel(nodeId, s.channel_id),
                        "共有を解除できませんでした",
                      )
                    }
                    className="flex size-9 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-destructive/10 hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 pointer-coarse:size-11"
                  >
                    {pending === s.channel_id ? (
                      <Loader2 className="size-4 animate-spin" aria-hidden />
                    ) : (
                      <X className="size-4" aria-hidden />
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

function kindLabel(kind: ChannelKind): string {
  return kind === "public" ? "公開チャンネル" : kind === "private" ? "非公開チャンネル" : "ダイレクトメッセージ";
}

function ChannelAvatar({ kind }: { kind: ChannelKind | null }) {
  const Icon = kind === "private" ? Lock : kind === "dm" ? MessageSquare : Hash;
  return (
    <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-secondary text-secondary-foreground">
      <Icon className="size-4" aria-hidden />
    </span>
  );
}
