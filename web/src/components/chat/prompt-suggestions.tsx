"use client";

import { FileText, ListChecks, Lightbulb, Sparkles } from "lucide-react";

import type { Suggestion } from "@/lib/home-modes";
import { seasonVar } from "@/lib/season";

/// アイコンは並び順に固定で、四季(shiki)に対応させる（春→夏→秋→冬）。
const ICONS = [FileText, ListChecks, Lightbulb, Sparkles] as const;

/// ホームの候補プロンプト（prompt-kit の PromptSuggestion 相当）。押すと即チャット開始。
/// 4 つの候補のアイコンに控えめな季節の差し色を添える。`meta`（出どころ）は本文の右に小さく出す。
export function PromptSuggestions({
  items,
  onPick,
}: {
  items: readonly Suggestion[];
  onPick: (text: string) => void;
}) {
  return (
    <div className="flex w-full max-w-2xl flex-wrap items-center justify-center gap-2">
      {items.slice(0, ICONS.length).map((s, i) => {
        const Icon = ICONS[i];
        // 季節トークンを CSS 変数に束ね、アイコン色とホバー時の枠/地のごく薄い差し色に使う。
        const season = seasonVar(i);
        return (
          <button
            key={s.text}
            type="button"
            onClick={() => onPick(s.text)}
            style={{ ["--season" as string]: season }}
            className="group inline-flex max-w-full items-center gap-2 rounded-full border border-border bg-card px-3.5 py-2 text-[13px] text-foreground/80 transition-colors hover:border-[var(--season)]/40 hover:bg-[var(--season)]/[0.07] hover:text-foreground"
          >
            <Icon
              className="size-4 shrink-0 transition-colors"
              style={{ color: season }}
              aria-hidden
            />
            <span className="min-w-0 truncate">{s.text}</span>
            {s.meta ? (
              <span className="shrink-0 text-[11.5px] text-muted-foreground">{s.meta}</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}
