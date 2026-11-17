"use client";

/// メッセージ画面の共有プリミティブ（アバター・チャンネルのアイコン・content blocks の描画）と、
/// 画面全体で共有する表示用の文脈（自分の id・エージェント名・名前の対応）。

import * as React from "react";
import Link from "next/link";
import { Bot, Hash, Lock, Megaphone, User } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { formatBytes } from "@/lib/format";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { NodeIcon } from "@/components/drive/primitives";
import { Markdown } from "@/components/prompt-kit/markdown";
import {
  agentIconUrl,
  authorName,
  mentionLabel,
  type AuthorView,
  type ChannelKind,
  type FileView,
  type MessageView,
} from "@/lib/messaging-api";

/// 画面全体で共有する表示用の文脈。
export type MessagingDisplay = {
  /// 自分の利用者 id。
  me: string;
  /// 自分のエージェントの名前（`@` の補完・`{kind:"agent"}` の表記）。
  agentName: string;
  /// 利用者 id → 表示名・`role:<id>` → ロール名（メンションの表記）。
  names: ReadonlyMap<string, string>;
  /// 自分のエージェントのアイコンが設定済みか。
  agentHasIcon: boolean;
  /// 自分のエージェントのアイコンを差し替えたときに変える値（キャッシュを外す）。
  iconVersion: number;
};

export const MessagingDisplayContext = React.createContext<MessagingDisplay>({
  me: "",
  agentName: "シキ",
  names: new Map(),
  agentHasIcon: false,
  iconVersion: 0,
});

export function useMessagingDisplay(): MessagingDisplay {
  return React.useContext(MessagingDisplayContext);
}

/// 利用者 id から決まる四季の巡回インデックス（アバターの色味が人ごとに揃う）。
function seasonOf(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h) % 4;
}

/// 表示名の頭文字（空なら「?」）。
function initialOf(name: string): string {
  const t = name.trim();
  return t ? Array.from(t)[0].toUpperCase() : "?";
}

/// 利用者のアバター（季節色のタイルに頭文字）。プロフィール画像の API は無いので頭文字で示す。
export function UserAvatar({
  userId,
  name,
  size = "default",
  className,
}: {
  userId: string;
  name: string;
  size?: "xs" | "sm" | "default" | "lg";
  className?: string;
}) {
  const tint = seasonVar(seasonOf(userId));
  return (
    <Avatar size={size} className={cn("rounded-[10px]", className)}>
      <AvatarFallback
        // 平らなパステルではなく、その人の季節色で淡いグラデ＋内側の細い縁。
        className="rounded-[inherit] font-semibold tracking-tight"
        style={{
          backgroundImage: `linear-gradient(140deg, color-mix(in oklab, ${tint} 38%, transparent), color-mix(in oklab, ${tint} 16%, transparent))`,
          color: `color-mix(in oklab, ${tint} 45%, var(--foreground))`,
          boxShadow: `inset 0 0 0 1px color-mix(in oklab, ${tint} 32%, transparent)`,
        }}
      >
        {initialOf(name)}
      </AvatarFallback>
    </Avatar>
  );
}

/// エージェント（AI）のアイコン。持ち主が設定していればその画像、無ければプライマリ地のロボット。
export function AgentAvatar({
  ownerId,
  hasIcon,
  size = "default",
  className,
}: {
  ownerId: string;
  hasIcon: boolean;
  size?: "xs" | "sm" | "default" | "lg";
  className?: string;
}) {
  const { me, iconVersion } = useMessagingDisplay();
  return (
    <Avatar size={size} className={cn("rounded-[10px]", className)}>
      {hasIcon ? (
        <AvatarImage src={agentIconUrl(ownerId, ownerId === me ? iconVersion : undefined)} alt="" />
      ) : null}
      <AvatarFallback
        className="rounded-[inherit] text-primary-foreground"
        style={{ backgroundImage: "linear-gradient(140deg, var(--primary), color-mix(in oklab, var(--primary) 70%, var(--season-autumn)))" }}
      >
        <Bot className="size-[60%]" strokeWidth={2.25} aria-hidden />
      </AvatarFallback>
    </Avatar>
  );
}

/// 発言の書き手のアバター（人とエージェントを描き分ける）。
export function AuthorAvatar({
  author,
  size = "default",
  className,
}: {
  author: AuthorView;
  size?: "xs" | "sm" | "default" | "lg";
  className?: string;
}) {
  return author.kind === "assistant" ? (
    <AgentAvatar ownerId={author.user_id} hasIcon={author.has_icon} size={size} className={className} />
  ) : (
    <UserAvatar userId={author.user_id} name={author.display_name} size={size} className={className} />
  );
}

/// 書き手の名前の行（エージェントは必ず「〇〇 のエージェント」を並べる・なりすまし対策）。
export function AuthorLabel({ author }: { author: AuthorView }) {
  if (author.kind === "assistant") {
    return (
      <>
        <span className="text-[13.5px] font-semibold text-foreground">{author.display_name}</span>
        <span
          className="rounded-full border border-border/60 px-1.5 py-px text-[10.5px] font-medium leading-4 text-muted-foreground"
          data-testid="agent-owner-label"
        >
          {author.owner_name ?? "利用者"} のエージェント
        </span>
      </>
    );
  }
  return (
    <span className={cn("text-[13.5px] font-semibold", author.active ? "text-foreground" : "text-muted-foreground")}>
      {authorName(author)}
    </span>
  );
}

/// チャンネル種別のアイコン。公開 = ハッシュ、非公開 = 錠、DM = 人、アナウンス = メガホン。
export function ChannelIcon({
  kind,
  announcement,
  agentDm,
  className,
}: {
  kind: ChannelKind;
  announcement?: boolean;
  agentDm?: boolean;
  className?: string;
}) {
  const cls = cn("size-4", className);
  if (kind === "dm") return agentDm ? <Bot className={cls} aria-hidden /> : <User className={cls} aria-hidden />;
  if (kind === "private") return <Lock className={cls} aria-hidden />;
  if (announcement) return <Megaphone className={cls} aria-hidden />;
  return <Hash className={cls} aria-hidden />;
}

const TILE_SIZE = {
  xs: "size-5 rounded-[6px] [&_svg]:size-3",
  sm: "size-7 rounded-[8px] [&_svg]:size-3.5",
  md: "size-9 rounded-[10px] [&_svg]:size-[18px]",
  lg: "size-14 rounded-[16px] [&_svg]:size-7",
} as const;

/// チャンネルのタイル（チャンネルごとの季節色の地に種別のアイコン）。人のアバターと同じ色の作りにして、
/// 一覧・ヘッダ・チャンネルの冒頭で同じチャンネルが同じ色で見分けられるようにする。
export function ChannelTile({
  channelId,
  kind,
  announcement,
  agentDm,
  size = "md",
  className,
}: {
  channelId: string;
  kind: ChannelKind;
  announcement?: boolean;
  agentDm?: boolean;
  size?: keyof typeof TILE_SIZE;
  className?: string;
}) {
  const tint = seasonVar(seasonOf(channelId));
  return (
    <span
      aria-hidden
      className={cn("flex shrink-0 items-center justify-center", TILE_SIZE[size], className)}
      style={{
        backgroundImage: `linear-gradient(140deg, color-mix(in oklab, ${tint} 34%, transparent), color-mix(in oklab, ${tint} 14%, transparent))`,
        color: `color-mix(in oklab, ${tint} 50%, var(--foreground))`,
        boxShadow: `inset 0 0 0 1px color-mix(in oklab, ${tint} 30%, transparent)`,
      }}
    >
      <ChannelIcon kind={kind} announcement={announcement} agentDm={agentDm} className="size-auto" />
    </span>
  );
}

/// `file_ref` の描画。**サーバが閲覧者の権限で評価した結果だけ**を受け取り、読めないものは名前も出さない。
export function FileRefCard({ file }: { file: FileView | undefined }) {
  if (!file || !file.available || !file.node_id) {
    return (
      <div className="mt-1.5 inline-flex max-w-full items-center gap-2.5 rounded-lg border border-dashed border-border bg-muted/40 px-3 py-2">
        <span className="flex size-8 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Lock className="size-4" aria-hidden />
        </span>
        <span className="min-w-0">
          <span className="block text-[13px] font-medium text-muted-foreground">参照できない添付</span>
          <span className="block text-[11.5px] text-muted-foreground/80">この文書の閲覧権限がありません</span>
        </span>
      </div>
    );
  }
  const size = file.size_bytes != null ? formatBytes(file.size_bytes) : null;
  return (
    <Link
      href={`/file/${encodeURIComponent(file.node_id)}`}
      className="mt-1.5 inline-flex max-w-full items-center gap-2.5 rounded-lg border border-border/60 bg-card/40 px-3 py-2 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      <span className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted/60">
        <NodeIcon kind="file" name={file.name ?? ""} contentType={file.content_type} className="size-5" />
      </span>
      <span className="min-w-0">
        <span className="block truncate text-[13px] font-medium text-foreground">{file.name ?? "ファイル"}</span>
        {size ? <span className="block truncate text-[11.5px] text-muted-foreground">{size}</span> : null}
      </span>
    </Link>
  );
}

/// メンションのチップ。自分あて（自分・`@channel`）は差し色で強調する。
export function MentionChip({ label, me }: { label: string; me: boolean }) {
  return (
    <span
      className={cn(
        "mx-px inline-flex items-center rounded px-1 py-px text-[13.5px] font-medium",
        me
          ? "bg-[color-mix(in_oklab,var(--season-autumn)_34%,transparent)] font-semibold text-foreground"
          : "bg-accent text-foreground/80",
      )}
    >
      {label}
    </span>
  );
}

/// content blocks（text / mention / file_ref）の描画。添付はサーバが評価した `files` と突き合わせる。
export function Blocks({ message }: { message: Pick<MessageView, "blocks" | "files" | "author"> }) {
  const { me, names, agentName } = useMessagingDisplay();
  const inline: React.ReactNode[] = [];
  const files: React.ReactNode[] = [];
  // エージェントへのメンションは「書いた人のエージェント」。自分の発言なら自分のエージェント名で出す。
  const agentLabel = message.author.kind === "user" && message.author.user_id === me ? agentName : "エージェント";
  message.blocks.forEach((b, i) => {
    if (b.type === "text") inline.push(<React.Fragment key={i}>{b.text}</React.Fragment>);
    else if (b.type === "mention") {
      const m = b.mention;
      const mine = (m.kind === "user" && m.user_id === me) || m.kind === "channel";
      inline.push(<MentionChip key={i} label={mentionLabel(m, names, agentLabel)} me={mine} />);
    } else if (b.type === "file_ref") {
      files.push(<FileRefCard key={i} file={message.files.find((f) => f.block_index === i)} />);
    }
  });
  // エージェントの回答は Markdown で書かれる（箇条書き・強調）。本文はチャンネルの発言（利用者が書いたもの）
  // から作られるので、利用者の本文と同じく untrusted で描く（画像を読まず、リンクには行き先のホストを添える）。
  const agentText =
    message.author.kind === "assistant" && message.blocks.every((b) => b.type === "text")
      ? message.blocks.map((b) => (b.type === "text" ? b.text : "")).join("")
      : null;
  return (
    <>
      {agentText !== null ? (
        <Markdown untrusted breaks className="text-[13.5px] leading-[1.65] text-foreground/90 [&>*:first-child]:mt-0 [&>*:last-child]:mb-0">
          {agentText}
        </Markdown>
      ) : inline.length > 0 ? (
        <p className="whitespace-pre-wrap break-words text-[13.5px] leading-[1.65] text-foreground/90">{inline}</p>
      ) : null}
      {files.length > 0 ? <div className="flex flex-col items-start">{files}</div> : null}
    </>
  );
}
