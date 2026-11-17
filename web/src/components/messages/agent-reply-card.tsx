"use client";

/// 自分だけに表示中のエージェントの回答（呼んだ発言の下に出す）。見た目は通常の発言の行と同じにし、
/// 「あなただけに表示」の印・操作・注意書きを本文の下に添える。
///
/// 「チャンネルに投稿」はまず確認なしで投げ、参照した資料のうち一部の参加者が読めないものがあれば
/// サーバが `posted: false` と件数を返すので、それを見せて確認を取ってから `confirmed: true` で投げ直す
/// （止めはしない — 黙って漏れることだけを防ぐ・design §4.14）。
///
/// 応答できなかった知らせ（`visibility: "failed"`）とチャットへの案内は回答ではないので、
/// 投稿の操作を出さず「閉じる」だけにする（サーバも投稿を拒否する）。

import * as React from "react";
import Link from "next/link";
import { AlertCircle, ArrowRight, EyeOff, MessageSquareText, Send, X } from "lucide-react";

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
import { timeLabel } from "./time";

export function AgentReplyCard({ reply, onChanged }: { reply: AgentReplyView; onChanged: () => void }) {
  const { me, agentName, agentHasIcon } = useMessagingDisplay();
  const [busy, setBusy] = React.useState<"publish" | "dismiss" | null>(null);
  const [warning, setWarning] = React.useState<NonNullable<PublishResult["warning"]> | null>(null);
  const failed = reply.visibility === "failed";
  const publishable = !failed && !reply.chat_thread_id;

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

  // 見た目は通常の発言と同じ行にする（アバター・名前・時刻・本文）。自分だけに見えていること・失敗したことは
  // 名前の行の印で示し、操作と注意書きは本文の下に置く。
  const author = { kind: "assistant" as const, user_id: me, display_name: agentName, owner_name: undefined, has_icon: agentHasIcon, active: true };
  return (
    <div
      className="group/msg relative px-5 pb-1.5 pt-2.5 transition-colors hover:bg-accent/35"
      data-testid="agent-reply-card"
      data-state={failed ? "failed" : reply.chat_thread_id ? "chat" : "reply"}
    >
      <div className="flex gap-2.5">
        <div className="w-8 shrink-0 pt-0.5">
          <AgentAvatar ownerId={me} hasIcon={agentHasIcon} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
            <span className="text-[13.5px] font-semibold text-foreground">{agentName}</span>
            {failed ? (
              <span className="inline-flex items-center gap-1 self-center rounded-full bg-destructive/10 px-1.5 py-px text-[10.5px] font-medium leading-4 text-destructive">
                <AlertCircle className="size-3" aria-hidden />
                応答できませんでした
              </span>
            ) : (
              <span className="inline-flex items-center gap-1 self-center rounded-full bg-muted px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground">
                <EyeOff className="size-3" aria-hidden />
                あなただけに表示
              </span>
            )}
            <time dateTime={reply.created_at} className="text-[11px] text-muted-foreground">
              {timeLabel(reply.created_at)}
            </time>
          </div>

          <Blocks message={{ blocks: reply.blocks, files: [], author }} />

          {reply.chat_thread_id ? (
            // チャット画面の成果物カードと同じ作り（タイル・題名・説明・開くボタン）。色は付けない。
            <div className="mt-2 flex max-w-xl items-center gap-3 rounded-xl border border-dashed border-border bg-card p-3">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground ring-1 ring-inset ring-border">
                <MessageSquareText className="size-[18px]" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium text-foreground">チャットで作業を続けています</span>
                <span className="mt-0.5 block text-[11.5px] text-muted-foreground">持ち主はあなたで、ほかの参加者には見えません</span>
              </span>
              <Link
                href={`/c/${encodeURIComponent(reply.chat_thread_id)}`}
                className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border px-3 py-1.5 text-[12px] font-medium outline-none transition-colors hover:border-ring/40 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                チャットを開く
                <ArrowRight className="size-3.5" aria-hidden />
              </Link>
            </div>
          ) : null}

          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
            {publishable ? (
              <Button size="sm" variant="outline" className="h-7 rounded-full px-3 text-[12px]" onClick={() => void publish(false)} loading={busy === "publish"} disabled={busy !== null}>
                <Send className="size-3.5" aria-hidden />
                チャンネルに投稿
              </Button>
            ) : null}
            <button
              type="button"
              onClick={() => void dismiss()}
              disabled={busy !== null}
              className="inline-flex h-7 items-center gap-1 rounded-full px-2 text-[12px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            >
              <X className="size-3.5" aria-hidden />
              {busy === "dismiss" ? "閉じています…" : "閉じる"}
            </button>
            <p className="min-w-0 basis-full text-[11px] leading-snug text-muted-foreground sm:basis-auto sm:flex-1">
              {failed
                ? "もう一度呼ぶと、新しく答えます。"
                : reply.chat_thread_id
                  ? "チャットはあなたの承認モードで動き、権限は増えません。"
                  : "あなたの権限で答えています。投稿するまで、ほかの参加者には見えません。"}
            </p>
          </div>
        </div>
      </div>

      <Dialog open={warning !== null} onOpenChange={(o) => (!o ? setWarning(null) : undefined)}>
        <DialogContent className="sm:max-w-[440px]">
          <DialogHeader>
            <DialogTitle>チャンネルに投稿しますか？</DialogTitle>
            <DialogDescription>
              {/* 参加者は全員読めても、公開チャンネルの投稿は参加していない組織メンバーも読める。 */}
              {(warning?.affected_members ?? 0) > 0
                ? `参照した資料のうち ${warning?.unreadable_sources ?? 0} 件は一部の参加者（${warning?.affected_members ?? 0} 人）が読めません。投稿しますか？`
                : `参照した資料のうち ${warning?.unreadable_sources ?? 0} 件は、組織の全員が読めるわけではありません。公開チャンネルの投稿は参加していない組織メンバーも読めます。投稿しますか？`}
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
