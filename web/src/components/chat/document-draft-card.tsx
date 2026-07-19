"use client";

/// AI が用意した未保存の下書き Word 文書のカード（#332・document_draft ブロック）。
///
/// note_draft / slide_draft / csv_draft カードと同型だが対象は Word 文書（.docx）。
/// カードから下書き文書画面（`/office/draft`）へ遷移し、本文を詰めてから
/// 「ドライブに保存」で .docx 化・確定する。下書き本文の真実源はクライアントの
/// 下書きストア（[`documentDraftStore`]）で、カードはその入口。

import { ArrowRight, FileText } from "lucide-react";
import Link from "next/link";

import { documentDraftHref, parseDocumentDraft } from "@/lib/documents/draft";

export function DocumentDraftCard({ raw, threadId }: { raw: unknown; threadId: string }) {
  const draft = parseDocumentDraft(raw);
  if (!draft) return null;
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border border-dashed bg-card p-3"
      data-testid="document-draft-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-400">
        <FileText className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="truncate text-sm font-medium">{draft.name}</span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          下書き Word 文書を用意しました。内容を確認・編集して「ドライブに保存」で確定します。
        </span>
      </span>
      <Link
        href={documentDraftHref(threadId, draft.name)}
        className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        下書きを開く
        <ArrowRight className="size-3.5" aria-hidden />
      </Link>
    </div>
  );
}
