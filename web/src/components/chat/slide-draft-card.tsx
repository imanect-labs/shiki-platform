"use client";

/// AI が用意した未保存の下書きスライドのカード（Task 11.3・slide_draft ブロック）。
///
/// note_draft カードと同型だが対象はスライド。カードから下書きスライド画面（`/slides/draft`）
/// へ遷移し、そこで内容を詰めてから「ドライブに保存」で確定する。下書き本文の真実源は
/// クライアントの下書きストア（[`slideDraftStore`]）で、カードはその入口。

import { ArrowRight, Presentation } from "lucide-react";
import Link from "next/link";

import { parseSlideDraft, slideDraftHref } from "@/lib/slides/draft";

/// 1 回の回答で出す下書きカードの位置（同じ名前は最後の 1 枚だけ）。
///
/// save_slide を append で数回に分けて呼ぶと、そのたびに**全体の**下書きが付く（3 枚・5 枚・8 枚…）。
/// 下書きストアは (会話, 名前) で 1 つなので、どのカードを開いても最新の下書きが開く。並べると
/// 別々の下書きを作ったように見えるため、最後（＝全体がそろった版）の 1 枚だけを出す。形の崩れた
/// ものは位置で残す（カード側で何も出さない）。
export function latestSlideDraftIndices(raws: unknown[]): Set<number> {
  const last = new Map<string, number>();
  raws.forEach((raw, i) => {
    const d = parseSlideDraft(raw);
    last.set(d ? `name:${d.name}` : `#${i}`, i);
  });
  return new Set(last.values());
}

export function SlideDraftCard({ raw, threadId }: { raw: unknown; threadId: string }) {
  const draft = parseSlideDraft(raw);
  if (!draft) return null;
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border border-dashed bg-card p-3"
      data-testid="slide-draft-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-amber-500/10 text-amber-600 dark:text-amber-400">
        <Presentation className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium" title={draft.name}>
          {draft.name}
        </span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          下書きスライドを用意しました。内容を確認・編集して「ドライブに保存」で確定します。
        </span>
      </span>
      <Link
        href={slideDraftHref(threadId, draft.name)}
        className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        下書きを開く
        <ArrowRight className="size-3.5" aria-hidden />
      </Link>
    </div>
  );
}
