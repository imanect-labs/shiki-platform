"use client";

/// AI が保存したノートの参照カード（Task 11P.5・note_ref ブロック）。
///
/// workflow_ref カードと同型。表示するのは StorageService へ作成済みの参照のみ
/// （backend の不変条件）。カードからノートページ（分割ビュー）へ遷移する。

import { ArrowRight, NotebookPen } from "lucide-react";
import Link from "next/link";

type NoteRef = {
  id: string;
  name: string;
};

/// 参照 JSON を防御的にパースする（形が崩れていたら描画しない）。
export function parseNoteRef(raw: unknown): NoteRef | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as { id?: unknown; name?: unknown };
  if (typeof r.id !== "string" || typeof r.name !== "string") return null;
  return { id: r.id, name: r.name };
}

export function NoteRefCard({ raw }: { raw: unknown }) {
  const note = parseNoteRef(raw);
  if (!note) return null;
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border bg-card p-3"
      data-testid="note-ref-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <NotebookPen className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="truncate text-sm font-medium">{note.name}</span>
        <span className="mt-0.5 block text-xs text-muted-foreground">
          ノートを保存しました。開いて共同編集できます。
        </span>
      </span>
      <Link
        href={`/notes/${note.id}`}
        className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        ノートを開く
        <ArrowRight className="size-3.5" aria-hidden />
      </Link>
    </div>
  );
}
