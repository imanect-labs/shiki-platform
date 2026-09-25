"use client";

/// メッセージ画面の共有プリミティブ（アバター・チャンネルのアイコン・content blocks の描画）。

import * as React from "react";
import { FileSpreadsheet, FileText, Hash, Lock, Presentation, User, Users } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  fileRefView,
  findMember,
  type DriveFile,
  type FileRefView,
  type ChannelKind,
  type MessageBlock,
} from "@/lib/messages-mock";

/// 利用者のアバター。顔写真があればそれを出し、無いとき（AI など）は季節色のタイルに
/// 頭文字を置く。`className` はアバター本体（枠の丸み・リング）に当たる。
/// 在席の点は本体の右下に重ねる。
export function MemberAvatar({
  memberId,
  size = "default",
  showPresence = false,
  className,
}: {
  memberId: string;
  size?: "xs" | "sm" | "default" | "lg";
  showPresence?: boolean;
  className?: string;
}) {
  const member = findMember(memberId);
  const tint = seasonVar(member.seasonIndex);
  return (
    <span className="relative inline-flex shrink-0">
      <Avatar size={size} className={cn("rounded-[10px]", className)}>
        {member.photo ? <AvatarImage src={member.photo} alt="" /> : null}
        <AvatarFallback
          // 平らなパステルではなく、その人の季節色で淡いグラデ＋内側の細い縁。
          // 文字は季節色を前景へ寄せて締め、地との差を保つ。
          className="rounded-[inherit] font-semibold tracking-tight"
          style={{
            backgroundImage: `linear-gradient(140deg, color-mix(in oklab, ${tint} 38%, transparent), color-mix(in oklab, ${tint} 16%, transparent))`,
            color: `color-mix(in oklab, ${tint} 45%, var(--foreground))`,
            boxShadow: `inset 0 0 0 1px color-mix(in oklab, ${tint} 32%, transparent)`,
          }}
        >
          {member.initial}
        </AvatarFallback>
      </Avatar>
      {showPresence ? (
        <span
          aria-hidden
          className={cn(
            "absolute -bottom-0.5 -right-0.5 z-10 size-2.5 rounded-full border-2 border-background",
            member.presence === "online"
              ? "bg-[oklch(0.72_0.15_150)]"
              : member.presence === "away"
                ? "bg-[oklch(0.78_0.13_80)]"
                : "bg-muted-foreground/40",
          )}
        />
      ) : null}
    </span>
  );
}

/// チャンネル種別のアイコン。公開 = ハッシュ、非公開 = 錠、DM = 相手のアバター（一覧側で差し替え）。
export function ChannelIcon({
  kind,
  memberCount,
  className,
}: {
  kind: ChannelKind;
  memberCount?: number;
  className?: string;
}) {
  if (kind === "private") return <Lock className={cn("size-4", className)} aria-hidden />;
  if (kind === "dm")
    return (memberCount ?? 2) > 2 ? (
      <Users className={cn("size-4", className)} aria-hidden />
    ) : (
      <User className={cn("size-4", className)} aria-hidden />
    );
  return <Hash className={cn("size-4", className)} aria-hidden />;
}

const FILE_ICONS = {
  sheet: FileSpreadsheet,
  doc: FileText,
  slide: Presentation,
  pdf: FileText,
} as const;

/// 文書の種別アイコン（添付候補・file_ref カードで共有する小さな角丸タイル）。
export function FileKindIcon({ kind }: { kind: DriveFile["kind"] }) {
  const Icon = FILE_ICONS[kind];
  return (
    <span
      className="flex size-8 shrink-0 items-center justify-center rounded-md"
      style={{
        backgroundColor: `color-mix(in oklab, ${seasonVar(1)} 16%, transparent)`,
        color: `color-mix(in oklab, ${seasonVar(1)} 70%, var(--foreground))`,
      }}
    >
      <Icon className="size-4" aria-hidden />
    </span>
  );
}

/// `file_ref` ブロックの描画。**file_id を持つだけで本文を複製しない**ため、
/// 表示のたびに閲覧側の権限を評価する（実装では StorageService 経由の ReBAC）。
///
/// 受け取るのは**評価済みの表示情報だけ**で、ACL（誰が読めるか）は渡らない。
/// 権限が無い相手にはファイル名・サイズ等のメタデータが手元に届かない形にしてある。
export function FileRefCard({ view }: { view: FileRefView }) {
  if (!view.readable) {
    return (
      <div className="mt-1.5 inline-flex max-w-full items-center gap-2.5 rounded-lg border border-dashed border-border bg-muted/40 px-3 py-2">
        <span className="flex size-8 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Lock className="size-4" aria-hidden />
        </span>
        <span className="min-w-0">
          <span className="block text-[13px] font-medium text-muted-foreground">
            参照できない添付
          </span>
          <span className="block text-[11.5px] text-muted-foreground/80">
            この文書の閲覧権限がありません
          </span>
        </span>
      </div>
    );
  }

  return (
    <button
      type="button"
      className="group/file mt-1.5 inline-flex max-w-full items-center gap-2.5 rounded-lg border border-border/60 bg-card/40 px-3 py-2 text-left outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      <FileKindIcon kind={view.kind} />
      <span className="min-w-0">
        <span className="block truncate text-[13px] font-medium text-foreground">{view.name}</span>
        <span className="block truncate text-[11.5px] text-muted-foreground">
          {view.location} ・ {view.size}
        </span>
      </span>
    </button>
  );
}

/// メンションのチップ。自分あては差し色で強調する。
export function MentionChip({ memberId, viewerId }: { memberId: string; viewerId: string }) {
  const member = findMember(memberId);
  const isMe = memberId === viewerId;
  return (
    <span
      className={cn(
        "mx-px inline-flex items-center rounded px-1 py-px text-[13.5px] font-medium",
        isMe
          ? "bg-[color-mix(in_oklab,var(--season-autumn)_34%,transparent)] font-semibold text-foreground"
          : "bg-accent text-foreground/80",
      )}
    >
      @{member.name}
    </span>
  );
}

/// content blocks（text / mention / file_ref）の描画。チャットとブロックの型を分けない。
export function Blocks({ blocks, viewerId }: { blocks: MessageBlock[]; viewerId: string }) {
  const inline = blocks.filter((b) => b.type !== "file_ref");
  const files = blocks.filter((b) => b.type === "file_ref");
  return (
    <>
      {inline.length > 0 ? (
        <p className="whitespace-pre-wrap break-words text-[13.5px] leading-[1.65] text-foreground/90">
          {inline.map((b, i) =>
            b.type === "text" ? (
              <React.Fragment key={i}>{b.text}</React.Fragment>
            ) : b.type === "mention" ? (
              <MentionChip key={i} memberId={b.member_id} viewerId={viewerId} />
            ) : null,
          )}
        </p>
      ) : null}
      {files.map((b, i) => {
        if (b.type !== "file_ref") return null;
        const view = fileRefView(b.node_id, viewerId);
        if (!view) return null;
        return (
          <div key={`${b.node_id}-${i}`}>
            <FileRefCard view={view} />
          </div>
        );
      })}
    </>
  );
}
