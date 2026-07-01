"use client";

import { FileText, ListChecks, Lightbulb, Sparkles } from "lucide-react";

import { seasonVar } from "@/lib/season";

/// ホームの候補プロンプト（prompt-kit の PromptSuggestion 相当）。押すと即チャット開始。
/// 4 つの候補を四季(shiki)に対応させ、アイコンに控えめな季節の差し色を添える（春→夏→秋→冬）。
const SUGGESTIONS: { icon: typeof FileText; text: string }[] = [
  { icon: FileText, text: "経費精算の提出期限と接待交際費の上限を教えて。" },
  { icon: ListChecks, text: "社内の経費精算のルールを教えて" },
  { icon: Lightbulb, text: "新機能の企画アイデアを5つ出して" },
  { icon: Sparkles, text: "この資料をもとにメール文面を作成して" },
];

export function PromptSuggestions({ onPick }: { onPick: (text: string) => void }) {
  return (
    <div className="flex w-full max-w-2xl flex-wrap items-center justify-center gap-2">
      {SUGGESTIONS.map((s, i) => {
        const Icon = s.icon;
        // 季節トークンを CSS 変数に束ね、アイコン色とホバー時の枠/地のごく薄い差し色に使う。
        const season = seasonVar(i);
        return (
          <button
            key={s.text}
            type="button"
            onClick={() => onPick(s.text)}
            style={{ ["--season" as string]: season }}
            className="group inline-flex items-center gap-2 rounded-full border border-border bg-card px-3.5 py-2 text-[13px] text-foreground/80 transition-colors hover:border-[var(--season)]/40 hover:bg-[var(--season)]/[0.07] hover:text-foreground"
          >
            <Icon
              className="size-4 transition-colors"
              style={{ color: season }}
              aria-hidden
            />
            {s.text}
          </button>
        );
      })}
    </div>
  );
}
