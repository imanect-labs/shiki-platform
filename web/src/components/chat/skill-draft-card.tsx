"use client";

/// AI が用意した未保存の下書きスキルのカード（#919・skill_draft ブロック）。
///
/// note_draft / csv_draft カードと同型だが対象はスキル。カードからスキルエディタ
/// （`/skills/new?thread=&name=`）を開き、内容を詰めてから人が保存する。
/// 下書きの真実源はクライアントの下書きストア（[`skillDraftStore`]）で、カードはその入口。

import { ArrowLeft, ArrowRight, Sparkles } from "lucide-react";
import Link from "next/link";
import * as React from "react";

import { parseSkillDraft, skillDraftHref, type AiSkillDraft } from "@/lib/skills/skill-draft";

/// スキルエディタのアシスタントパネルの中では、カードを「遷移」ではなく「エディタに反映」にする
/// （過去の案へ戻す・手で直した後にもう一度当てる用途。エディタを開いたまま別の新規スキル画面へ連れて行かない）。
export const SkillDraftApplyContext = React.createContext<((draft: AiSkillDraft) => void) | null>(
  null,
);

const ACTION =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:min-h-10";

export function SkillDraftCard({ raw, threadId }: { raw: unknown; threadId: string }) {
  const apply = React.useContext(SkillDraftApplyContext);
  const draft = parseSkillDraft(raw);
  if (!draft) return null;
  return (
    <div
      className="my-2 flex items-center gap-3 rounded-xl border border-dashed bg-card p-3"
      data-testid="skill-draft-card"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-season-winter/12 text-[color-mix(in_oklab,var(--season-winter)_70%,var(--foreground))]">
        <Sparkles className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium" title={draft.name}>
          {draft.name}
        </span>
        <span className="mt-0.5 line-clamp-2 block text-xs text-muted-foreground">
          {(draft.fields?.includes("description") ?? true) && draft.body.description
            ? draft.body.description
            : "スキルの下書きを用意しました。"}
        </span>
      </span>
      {apply ? (
        <button
          type="button"
          onClick={() => apply(draft)}
          className={ACTION}
          title="この案の内容でエディタを置き換える"
          data-testid="skill-draft-apply"
        >
          <ArrowLeft className="size-3.5" aria-hidden />
          エディタに反映
        </button>
      ) : (
        <Link href={skillDraftHref(threadId, draft.name)} className={ACTION}>
          エディタで開く
          <ArrowRight className="size-3.5" aria-hidden />
        </Link>
      )}
    </div>
  );
}
