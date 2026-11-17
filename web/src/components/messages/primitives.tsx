"use client";

/// メッセージ画面の共有プリミティブ（アバター・チャンネルのアイコン・content blocks の描画）と、
/// 画面全体で共有する表示用の文脈（自分の id・エージェント名・名前の対応）。

import * as React from "react";
import Link from "next/link";
import { Bot, Hash, Lock, Megaphone, Sparkles, User } from "lucide-react";

import { cn } from "@/lib/utils";
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

/// エージェントの回答の外部リンクの出所（無し＝外部リンクはすべて押したときに確認する）。
const NO_LINK_SOURCES: ReadonlySet<string> = new Set();

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

/// 表示名の頭文字（空なら「?」）。
function initialOf(name: string): string {
  const t = name.trim();
  return t ? Array.from(t)[0].toUpperCase() : "?";
}

/// 利用者のアバター（中立の淡い地に頭文字）。プロフィール画像の API は無いので頭文字で示す。
/// 色で人を塗り分けない（名前の行で見分けられ、色だけが騒がしくなるため）。
export function UserAvatar({
  name,
  size = "default",
  className,
}: {
  /// 呼び出し側の互換のため受け取る（色で人を塗り分けないので見た目には使わない）。
  userId?: string;
  name: string;
  size?: "xs" | "sm" | "default" | "lg";
  className?: string;
}) {
  return (
    <Avatar size={size} className={cn("rounded-[10px]", className)}>
      <AvatarFallback className="rounded-[inherit] bg-muted font-medium text-muted-foreground ring-1 ring-inset ring-border">{initialOf(name)}</AvatarFallback>
    </Avatar>
  );
}

/// エージェント（AI）のアイコン。持ち主が設定していればその画像、無ければ中立の淡い地に細い線のロボット。
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
      <AvatarFallback className="rounded-[inherit] bg-muted text-muted-foreground ring-1 ring-inset ring-border">
        <Bot className="size-[58%]" strokeWidth={1.75} aria-hidden />
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

/// AI の発言の印（名前の横に置く・人の発言と見分けるため。色は付けない）。
export function AiBadge() {
  return (
    <span
      className="inline-flex items-center gap-0.5 self-center rounded-[5px] border border-foreground/25 px-1 py-px text-[10px] font-semibold leading-[14px] tracking-wide text-foreground/75"
      data-testid="ai-badge"
    >
      <Sparkles className="size-2.5" aria-hidden />
      AI
    </span>
  );
}

/// 書き手の名前の行（エージェントは必ず「〇〇 のエージェント」を並べる・なりすまし対策）。
export function AuthorLabel({ author }: { author: AuthorView }) {
  if (author.kind === "assistant") {
    return (
      <>
        <span className="text-[13.5px] font-semibold text-foreground">{author.display_name}</span>
        <AiBadge />
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
  xs: "size-5 [&_svg]:size-4",
  sm: "size-7 [&_svg]:size-[18px]",
  md: "size-9 [&_svg]:size-5",
  lg: "size-10 [&_svg]:size-7",
} as const;

/// チャンネルの種別アイコン（地も枠も付けない細い線のアイコン・大きさだけを揃える）。
export function ChannelTile({
  kind,
  announcement,
  agentDm,
  size = "md",
  className,
}: {
  /// 呼び出し側の互換のため受け取る（見た目には使わない）。
  channelId?: string;
  kind: ChannelKind;
  announcement?: boolean;
  agentDm?: boolean;
  size?: keyof typeof TILE_SIZE;
  className?: string;
}) {
  return (
    <span aria-hidden className={cn("flex shrink-0 items-center justify-center text-muted-foreground", TILE_SIZE[size], className)}>
      <ChannelIcon kind={kind} announcement={announcement} agentDm={agentDm} className="size-auto [stroke-width:1.75]" />
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
  // エージェントの回答は Markdown（箇条書き・強調・```ui の図）で書かれる。図はチャットの回答と同じく描き、
  // ボタンは置き場所（発言の行の YuiActionContext）が押せるかを決める。本文はチャンネルの発言（利用者が
  // 書いたもの）を元に作られるので、外部の画像は読まず、外部リンクは押したときに確認してから開く。
  const agentText =
    message.author.kind === "assistant" && message.blocks.every((b) => b.type === "text")
      ? message.blocks.map((b) => (b.type === "text" ? b.text : "")).join("")
      : null;
  return (
    <>
      {agentText !== null ? (
        <Markdown
          linkSources={NO_LINK_SOURCES}
          className="text-[13.5px] leading-[1.65] text-foreground/90 [&>*:first-child]:mt-0 [&>*:last-child]:mb-0"
        >
          {agentText}
        </Markdown>
      ) : inline.length > 0 ? (
        <p className="whitespace-pre-wrap break-words text-[13.5px] leading-[1.65] text-foreground/90">{inline}</p>
      ) : null}
      {files.length > 0 ? <div className="flex flex-col items-start">{files}</div> : null}
    </>
  );
}
