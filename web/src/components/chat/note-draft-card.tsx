"use client";

/// AI が用意した未保存の下書きノートのカード（issue #282・note_draft ブロック）。
///
/// note_ref カードと同型だが、**まだ StorageService へ未作成**の下書き。カードから下書きノート
/// 画面（`/notes/draft`）へ遷移し、そこで内容を詰めてから「ドライブに保存」で確定する。
/// 下書き本文の真実源はクライアントの下書きストア（[`draft-store`]）で、カードはその入口。

import { ArrowRight, PencilLine } from "lucide-react";
import Link from "next/link";

import { draftHref } from "@/lib/notes/draft-nav";
import { parseNoteDraft } from "@/lib/notes/draft-store";

export function NoteDraftCard({ raw, threadId }: { raw: unknown; threadId: string }) {
  const draft = parseNoteDraft(raw);
  if (!draft) return null;
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border border-dashed bg-card p-3"
      data-testid="note-draft-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-400">
        <PencilLine className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="truncate text-sm font-medium">{draft.name}</span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          下書きを用意しました。内容を確認・編集して「ドライブに保存」で確定します。
        </span>
      </span>
      <Link
        href={draftHref(threadId, draft.name)}
        className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        下書きを開く
        <ArrowRight className="size-3.5" aria-hidden />
      </Link>
    </div>
  );
}
