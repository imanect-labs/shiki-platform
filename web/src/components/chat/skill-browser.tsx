"use client";

/// 「+」メニューのスキル（スラッシュコマンド）の並びと 1 行（#390）。
///
/// メニューには最近使ったもの・先頭の数件だけを出す。探すのは「スキルを検索」＝入力欄に `/` を
/// 入れて開く補完に一本化している（検索の面を 2 つ持たない）。

import * as React from "react";
import { Sparkles } from "lucide-react";

import { commandLabel, type SlashSuggestion } from "@/lib/slash-command";
import { readScoped, writeScoped } from "@/lib/user-storage";

/// 最近使ったスキル（ユーザー＋テナント単位のキー・ログアウトで消える・#817）。
const RECENT_KEY = "shiki.composer.recent-skills";
const MAX_RECENT = 5;

function readRecent(): string[] {
  try {
    const raw = readScoped(RECENT_KEY);
    const v: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/// 選んだスキルを「最近」の先頭へ（端末ごとの便宜。保存できない環境では何もしない）。
export function recordRecentSkill(key: string): void {
  try {
    const next = [key, ...readRecent().filter((k) => k !== key)].slice(0, MAX_RECENT);
    writeScoped(RECENT_KEY, JSON.stringify(next));
  } catch {
    /* プライベートモード等では保存しない */
  }
}

/// 最近使ったものを先頭にした並び（残りはカタログの並び順のまま）。
export function useRankedSkills(skills: SlashSuggestion[], open: boolean): SlashSuggestion[] {
  const [recent, setRecent] = React.useState<string[]>([]);
  // メニューを開くたびに読み直す（別の入力欄で選んだ分も反映する）。
  React.useEffect(() => {
    if (open) setRecent(readRecent());
  }, [open]);
  return React.useMemo(() => {
    const byKey = new Map(skills.map((s) => [s.key, s]));
    const head = recent.map((k) => byKey.get(k)).filter((s): s is SlashSuggestion => !!s);
    const seen = new Set(head.map((s) => s.key));
    return [...head, ...skills.filter((s) => !seen.has(s.key))];
  }, [skills, recent]);
}

/// スキル 1 行（`/コマンド`・スキル名・説明）。
export function SkillRow({
  skill,
  onPick,
  testId,
}: {
  skill: SlashSuggestion;
  onPick: (s: SlashSuggestion) => void;
  testId?: string;
}) {
  return (
    <button
      type="button"
      onClick={() => onPick(skill)}
      data-testid={testId}
      className="flex min-h-12 w-full items-start gap-3 rounded-xl px-3 py-2.5 text-left transition-colors hover:bg-accent active:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:min-h-0 md:py-2"
    >
      <Sparkles className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className="block truncate text-[15px] font-medium text-foreground md:text-sm">
            {commandLabel(skill)}
          </span>
          {/* コマンドにはどのスキルかを添える（同じコマンド名を別スキルが宣言できるため）。 */}
          {skill.token ? (
            <span className="shrink-0 truncate text-[11.5px] text-muted-foreground">{skill.skillName}</span>
          ) : null}
        </span>
        <span className="mt-0.5 block line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">
          {skill.summary}
        </span>
      </span>
    </button>
  );
}
