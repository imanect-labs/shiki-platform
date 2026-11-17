"use client";

/// 1 発言の行。ホバー（とキーボードのフォーカス）で操作（リアクション・スレッド返信・編集・削除）が浮く。

import * as React from "react";
import { Link2, MessageSquareReply, Pencil, Smile, Trash2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { invokeMessageUiAction, mentionLabel, type MemberView, type MessageBlock, type MessageView } from "@/lib/messaging-api";
import { YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import { AuthorAvatar, AuthorLabel, Blocks, useMessagingDisplay } from "./primitives";
import { Composer, draftOf } from "./composer";
import { timeLabel } from "./time";

const QUICK_EMOJI = ["👍", "🙏", "✅", "🎉", "👀", "🔥", "😄", "💡"];

const TOOL_BTN =
  "flex size-7 items-center justify-center rounded-[7px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

export function ReactionPills({
  message,
  onToggle,
}: {
  message: MessageView;
  onToggle: (emoji: string, on: boolean) => void;
}) {
  if (message.reactions.length === 0) return null;
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-1">
      {message.reactions.map((r) => {
        const others = r.count - (r.me ? 1 : 0);
        const who = r.me ? (others > 0 ? `あなたと他 ${others} 人` : "あなた") : `${r.count} 人`;
        return (
          <Tooltip key={r.emoji}>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => onToggle(r.emoji, !r.me)}
                aria-pressed={r.me}
                aria-label={`${r.emoji} ${r.count} 件${r.me ? "（自分も付けています）" : ""}`}
                className={cn(
                  "flex h-6 items-center gap-1 rounded-full border px-2 text-[11.5px] leading-none outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  r.me
                    ? "border-transparent bg-accent font-semibold text-foreground"
                    : "border-border/60 bg-card/40 text-muted-foreground hover:bg-accent",
                )}
              >
                <span className="text-[12.5px]">{r.emoji}</span>
                {r.count}
              </button>
            </TooltipTrigger>
            <TooltipContent side="top">{who}</TooltipContent>
          </Tooltip>
        );
      })}
    </div>
  );
}

function HoverToolbar({
  onReact,
  onReply,
  onEdit,
  onDelete,
}: {
  onReact?: (emoji: string) => void;
  onReply?: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  const [pickerOpen, setPickerOpen] = React.useState(false);
  if (!onReact && !onReply && !onEdit && !onDelete) return null;
  return (
    <div
      role="toolbar"
      aria-label="発言の操作"
      className={cn(
        // 行の内側に収める。行の外（-top-3）へ出すと、上の発言のリアクション行や区切り線に重なる。
        "absolute right-3 top-0 z-10 flex items-center gap-0.5 rounded-[9px] border border-border/70 bg-popover p-0.5 shadow-sm",
        // 見えていない間はクリックを奪わない（上の行の右上に不可視の的が重なるのを防ぐ）。
        "pointer-events-none opacity-0 transition-opacity duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        "group-hover/msg:pointer-events-auto group-hover/msg:opacity-100 focus-within:pointer-events-auto focus-within:opacity-100",
        // タッチ端末は hover が無い。全行に常に出すと名前や本文に重なるので、行をタップした（行に
        // フォーカスがある）ときだけ出す。指で押せる大きさにする。
        "group-focus-within/msg:pointer-events-auto group-focus-within/msg:opacity-100 pointer-coarse:[&_button]:size-10",
        pickerOpen && "pointer-events-auto opacity-100",
      )}
    >
      {onReact ? (
        <Popover open={pickerOpen} onOpenChange={setPickerOpen}>
          <PopoverTrigger asChild>
            <button type="button" aria-label="リアクションを付ける" className={TOOL_BTN}>
              <Smile className="size-4" aria-hidden />
            </button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-auto p-1">
            <div className="grid grid-cols-4 gap-0.5" role="group" aria-label="絵文字">
              {QUICK_EMOJI.map((e) => (
                <button
                  key={e}
                  type="button"
                  aria-label={e}
                  onClick={() => {
                    onReact(e);
                    setPickerOpen(false);
                  }}
                  className="flex size-8 items-center justify-center rounded-md text-[16px] outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {e}
                </button>
              ))}
            </div>
          </PopoverContent>
        </Popover>
      ) : null}
      {onReply ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" onClick={onReply} aria-label="スレッドで返信" className={TOOL_BTN}>
              <MessageSquareReply className="size-4" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="top">スレッドで返信</TooltipContent>
        </Tooltip>
      ) : null}
      {onEdit ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" onClick={onEdit} aria-label="編集" className={TOOL_BTN}>
              <Pencil className="size-4" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="top">編集</TooltipContent>
        </Tooltip>
      ) : null}
      {onDelete ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" onClick={onDelete} aria-label="削除" className={cn(TOOL_BTN, "hover:text-destructive")}>
              <Trash2 className="size-4" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="top">削除</TooltipContent>
        </Tooltip>
      ) : null}
    </div>
  );
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function LinkPreviews({ message }: { message: MessageView }) {
  if (message.link_previews.length === 0) return null;
  return (
    <div className="mt-1.5 flex flex-col gap-1.5">
      {message.link_previews.map((p) => (
        <a
          key={p.url}
          href={p.url}
          target="_blank"
          rel="noopener noreferrer nofollow"
          className="block max-w-md rounded-lg border-l-2 border-border bg-card/40 px-3 py-2 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
            <Link2 className="size-3" aria-hidden />
            <span className="truncate">{p.site_name ?? hostOf(p.url)}</span>
          </span>
          {p.title ? <span className="mt-0.5 block truncate text-[13px] font-medium text-foreground">{p.title}</span> : null}
          {p.description ? (
            <span className="mt-0.5 line-clamp-2 block text-[12px] text-muted-foreground">{p.description}</span>
          ) : null}
        </a>
      ))}
    </div>
  );
}

export function MessageRow({
  message,
  grouped,
  unreadBoundary,
  compact,
  members,
  canModerate,
  canReact,
  onReact,
  onOpenThread,
  onEdit,
  onDelete,
  children,
}: {
  message: MessageView;
  /// 直前の発言と同じ人・近い時刻なら名前とアバターを省く（連続表示）。
  grouped: boolean;
  unreadBoundary?: boolean;
  /// スレッドペイン内の表示（返信数サマリと返信ボタンを出さない）。
  compact?: boolean;
  /// 編集時のメンション候補。
  members: MemberView[];
  /// チャンネルの管理者（他人の発言も削除できる）。
  canModerate: boolean;
  /// リアクションできるか（参加者・アーカイブされていない）。
  canReact: boolean;
  onReact: (emoji: string, on: boolean) => void;
  onOpenThread?: () => void;
  onEdit: (blocks: MessageBlock[]) => Promise<boolean>;
  onDelete: () => Promise<void>;
  /// 行の下に差し込むもの（自分だけに表示の回答）。
  children?: React.ReactNode;
}) {
  const { me, names, agentName } = useMessagingDisplay();
  const [editing, setEditing] = React.useState(false);
  const [confirmDelete, setConfirmDelete] = React.useState(false);
  const [deleting, setDeleting] = React.useState(false);
  const alive = message.state === "normal";
  const mine = message.author.kind === "user" && message.author.user_id === me;
  const time = timeLabel(message.created_at);
  const replyCount = message.reply_count;

  // エージェントの回答の図（```ui）のボタン。押した内容は自分の発言としてスレッドに入り、エージェントが
  // 続けて答える（サーバーが保存済みの回答で検証する）。押せるのは自分のエージェントの回答だけ。
  const yuiHost = React.useMemo<YuiActionHost | null>(() => {
    if (message.author.kind !== "assistant") return null;
    const owner = message.author.user_id === me;
    return {
      invoke: (key, values) => invokeMessageUiAction(message.id, key, values),
      isBound: () => false,
      invokedActions: [],
      uiRuns: [],
      onChanged: () => undefined,
      disabledReason: !alive
        ? "この回答のボタンは使えません"
        : !owner
          ? `${message.author.owner_name ?? "持ち主"} のエージェントの回答なので、ボタンは持ち主だけが押せます`
          : !canReact
            ? "このチャンネルでは投稿できないため、ボタンは使えません"
            : undefined,
    };
  }, [message.author, message.id, me, alive, canReact]);

  const draft = React.useMemo(
    () => (editing ? draftOf(message.blocks, message.files, (m) => mentionLabel(m, names, agentName)) : undefined),
    [editing, message.blocks, message.files, names, agentName],
  );

  return (
    <>
      {unreadBoundary ? (
        <div className="relative my-2 flex items-center gap-2 px-5" role="separator" aria-label="ここから未読">
          <span className="h-px flex-1 bg-[color-mix(in_oklab,var(--season-spring)_70%,transparent)]" />
          <span
            className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold leading-none"
            style={{
              backgroundColor: `color-mix(in oklab, ${seasonVar(0)} 26%, transparent)`,
              color: `color-mix(in oklab, ${seasonVar(0)} 42%, var(--foreground))`,
            }}
          >
            ここから未読
          </span>
        </div>
      ) : null}

      <div
        className={cn(
          "group/msg relative px-5 outline-none transition-colors hover:bg-accent/35 focus-within:bg-accent/35",
          grouped ? "py-0.5" : "pb-0.5 pt-2.5",
        )}
        data-testid="message-row"
        data-message-id={message.id}
        // タップで行にフォーカスを置き、タッチ端末でも操作（リアクション・返信・編集・削除）を出せるようにする。
        tabIndex={-1}
      >
        {!editing ? (
          <HoverToolbar
            onReact={alive && canReact ? (e) => onReact(e, !message.reactions.some((r) => r.emoji === e && r.me)) : undefined}
            onReply={compact || !alive ? undefined : onOpenThread}
            onEdit={alive && mine ? () => setEditing(true) : undefined}
            onDelete={alive && (mine || canModerate) ? () => setConfirmDelete(true) : undefined}
          />
        ) : null}
        {/* 描画の省略（content-visibility）は本文側だけに掛ける。行に掛けると paint containment で
            絶対配置の操作バーが行の高さで切れる（1 行の連続発言・タッチ端末の大きいボタン）。 */}
        <div className="flex gap-2.5 [contain-intrinsic-size:auto_48px] [content-visibility:auto]">
          <div className="w-8 shrink-0 pt-0.5">
            {grouped ? (
              <span className="hidden select-none pt-1 text-[10.5px] leading-none text-muted-foreground/70 group-hover/msg:block">
                {time}
              </span>
            ) : (
              <AuthorAvatar author={message.author} />
            )}
          </div>

          <div className="min-w-0 flex-1">
            {!grouped ? (
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <AuthorLabel author={message.author} />
                <time dateTime={message.created_at} className="text-[11px] text-muted-foreground">
                  {time}
                </time>
              </div>
            ) : null}

            {message.state === "deleted" ? (
              <p className="text-[13px] italic text-muted-foreground">この発言は削除されました</p>
            ) : message.state === "expired" ? (
              <p className="text-[13px] italic text-muted-foreground">この発言は保存期間を過ぎました</p>
            ) : editing && draft ? (
              <Composer
                placeholder="発言を編集"
                members={members}
                allowChannelMention={false}
                initial={draft}
                submitLabel="保存"
                autoFocus
                onCancel={() => setEditing(false)}
                onSubmit={async (blocks) => {
                  const ok = await onEdit(blocks);
                  if (ok) setEditing(false);
                  return ok;
                }}
              />
            ) : (
              <>
                <YuiActionContext.Provider value={yuiHost}>
                  <Blocks message={message} />
                </YuiActionContext.Provider>
                {message.edited ? <span className="text-[11px] text-muted-foreground">（編集済み）</span> : null}
                <LinkPreviews message={message} />
              </>
            )}

            {alive ? <ReactionPills message={message} onToggle={canReact ? onReact : () => undefined} /> : null}

            {!compact && replyCount > 0 ? (
              <button
                type="button"
                onClick={onOpenThread}
                className="mt-1.5 flex items-center gap-2 rounded-md py-1 pr-2 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                {/* 差し色は四季トークン。text-primary はライトで本文と見分けが付かないため使わない。 */}
                <span className="text-[12px] font-medium" style={{ color: seasonVar(currentSeasonIndex()) }}>
                  返信 {replyCount} 件
                </span>
                {message.last_reply_at ? (
                  <span className="text-[11.5px] text-muted-foreground">最終 {timeLabel(message.last_reply_at)}</span>
                ) : null}
              </button>
            ) : null}
          </div>
        </div>
      </div>
      {children}

      <Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <DialogContent className="sm:max-w-[420px]">
          <DialogHeader>
            <DialogTitle>発言を削除しますか？</DialogTitle>
            <DialogDescription>
              「この発言は削除されました」と表示されます。スレッドの返信は残ります。
              {!mine ? " 管理者として他の人の発言を削除します。" : ""}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirmDelete(false)} disabled={deleting}>
              キャンセル
            </Button>
            <Button
              variant="destructive"
              loading={deleting}
              onClick={async () => {
                setDeleting(true);
                try {
                  await onDelete();
                  setConfirmDelete(false);
                } finally {
                  setDeleting(false);
                }
              }}
            >
              削除する
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
