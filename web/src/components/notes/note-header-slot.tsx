"use client";

/// ノート詳細の統一ヘッダ注入（横バー二重の解消）。ページ本文からこのコンポーネントを
/// 描画すると、戻る/名前/閲覧のみ/プレゼンス/同期ピル/アシスタント切替を共通ヘッダへ寄せる。
/// null を返すだけなのでレイアウトには影響しない。

import { ArrowLeft, Eye, Sparkles } from "lucide-react";
import Link from "next/link";

import { Button } from "@/components/ui/button";
import { PresenceAvatars } from "@/components/notes/presence";
import { usePageHeader } from "@/components/shell/page-header-context";
import type { CollabProvider, CollabStatus } from "@/lib/collab";
import { SyncPill } from "./note-status";

export function NoteSyncSlot({
  name,
  editable,
  status,
  synced,
  chatOpen,
  onToggleChat,
  provider,
}: {
  name: string;
  editable: boolean;
  status: CollabStatus;
  synced: boolean;
  chatOpen: boolean;
  onToggleChat: () => void;
  provider: CollabProvider | null;
}) {
  usePageHeader(
    () => (
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <Link
          href="/drive"
          className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-95"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{name}</span>
        {!editable ? (
          <span
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium text-muted-foreground"
            data-testid="note-readonly-badge"
          >
            <Eye className="size-3.5" aria-hidden />
            閲覧のみ
          </span>
        ) : null}
        {provider ? <PresenceAvatars provider={provider} /> : null}
        <SyncPill status={status} synced={synced} />
        <Button
          type="button"
          variant={chatOpen ? "secondary" : "ghost"}
          size="sm"
          onClick={onToggleChat}
          aria-pressed={chatOpen}
          data-testid="note-ask-ai"
        >
          <Sparkles className="mr-1.5 size-4" aria-hidden />
          AI に依頼
        </Button>
      </div>
    ),
    [name, editable, status, synced, chatOpen, onToggleChat, provider],
  );
  return null;
}
