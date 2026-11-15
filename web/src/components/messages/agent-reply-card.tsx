"use client";

/// 自分だけに表示中のエージェントの回答（呼んだ発言の下に出す）。
///
/// 「チャンネルに投稿」はまず確認なしで投げ、参照した資料のうち一部の参加者が読めないものがあれば
/// サーバが `posted: false` と件数を返すので、それを見せて確認を取ってから `confirmed: true` で投げ直す
/// （止めはしない — 黙って漏れることだけを防ぐ・design §4.14）。

import * as React from "react";
import Link from "next/link";
import { EyeOff, MessageSquareText, Send, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import {
  dismissAgentReply,
  publishAgentReply,
  type AgentReplyView,
  type PublishResult,
} from "@/lib/messaging-api";
import { AgentAvatar, Blocks, useMessagingDisplay } from "./primitives";

export function AgentReplyCard({ reply, onChanged }: { reply: AgentReplyView; onChanged: () => void }) {
  const { me, agentName, agentHasIcon } = useMessagingDisplay();
  const [busy, setBusy] = React.useState<"publish" | "dismiss" | null>(null);
  const [warning, setWarning] = React.useState<NonNullable<PublishResult["warning"]> | null>(null);

  const publish = async (confirmed: boolean) => {
    setBusy("publish");
    try {
      const r = await publishAgentReply(reply.id, confirmed);
      if (!r.posted && r.warning) {
        setWarning(r.warning);
        return;
      }
      setWarning(null);
      toast({ title: "チャンネルに投稿しました" });
      onChanged();
    } catch (e) {
      toast({ title: "投稿できませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setBusy(null);
    }
  };

  const dismiss = async () => {
    setBusy("dismiss");
    try {
      await dismissAgentReply(reply.id);
      onChanged();
    } catch (e) {
      toast({ title: "閉じられませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="mx-5 my-1.5 rounded-xl border border-dashed border-border bg-card/40 px-3 py-2.5" data-testid="agent-reply-card">
      <div className="flex items-center gap-2">
        <AgentAvatar ownerId={me} hasIcon={agentHasIcon} size="sm" />
        <span className="text-[13px] font-semibold text-foreground">{agentName}</span>
        <span className="flex items-center gap-1 rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
          <EyeOff className="size-3" aria-hidden />
          自分だけに表示
        </span>
      </div>
      <div className="mt-1.5 pl-9">
        <Blocks message={{ blocks: reply.blocks, files: [], author: { kind: "assistant", user_id: me, display_name: agentName, has_icon: agentHasIcon, active: true } }} />
        {reply.chat_thread_id ? (
          <Link
            href={`/c/${encodeURIComponent(reply.chat_thread_id)}`}
            className="mt-2 inline-flex items-center gap-1.5 rounded-lg border border-border/60 bg-card/40 px-2.5 py-1.5 text-[12px] font-medium text-foreground outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
          >
            <MessageSquareText className="size-3.5" aria-hidden />
            チャットで続きを開く
          </Link>
        ) : null}
        <p className="mt-2 text-[11px] leading-snug text-muted-foreground">
          あなたの権限で答えています。チャンネルに投稿するまで、ほかの参加者には見えません。
        </p>
        <div className="mt-2 flex flex-wrap gap-1.5">
          <Button size="sm" onClick={() => void publish(false)} loading={busy === "publish"} disabled={busy !== null}>
            <Send className="size-3.5" aria-hidden />
            チャンネルに投稿
          </Button>
          <Button size="sm" variant="ghost" onClick={() => void dismiss()} loading={busy === "dismiss"} disabled={busy !== null}>
            <X className="size-3.5" aria-hidden />
            閉じる
          </Button>
        </div>
      </div>

      <Dialog open={warning !== null} onOpenChange={(o) => (!o ? setWarning(null) : undefined)}>
        <DialogContent className="sm:max-w-[440px]">
          <DialogHeader>
            <DialogTitle>チャンネルに投稿しますか？</DialogTitle>
            <DialogDescription>
              参照した資料のうち {warning?.unreadable_sources ?? 0} 件は一部の参加者（
              {warning?.affected_members ?? 0} 人）が読めません。投稿しますか？
            </DialogDescription>
          </DialogHeader>
          <p className="text-[12.5px] leading-relaxed text-muted-foreground">
            投稿すると、回答の本文はチャンネルの全員に見えます。資料そのものの権限は変わりません。
          </p>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setWarning(null)} disabled={busy !== null}>
              やめる
            </Button>
            <Button onClick={() => void publish(true)} loading={busy === "publish"}>
              投稿する
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
