"use client";

/// スキル（スラッシュコマンド）の一覧と絞り込み（#390）。
///
/// 「+」メニューには最近使ったもの・先頭の数件だけを出し、残りは「すべてのスキル」から
/// この一覧（スマホは下からのシート・PC は中央のダイアログ）で探す。選ぶと入力欄に
/// コマンドが確定する（実行はしない＝入力欄で `/` を打っても呼べることを学べる）。

import * as React from "react";
import { Search, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import type { SlashSuggestion } from "@/lib/slash-command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

const RECENT_KEY = "shiki.composer.recent-skills";
const MAX_RECENT = 5;

function readRecent(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENT_KEY);
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
    window.localStorage.setItem(RECENT_KEY, JSON.stringify(next));
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

const norm = (s: string) => s.normalize("NFKC").toLowerCase();

/// コマンド名・スキル名・説明のどれかに、空白区切りの語がすべて含まれるか。
export function filterSkills(skills: SlashSuggestion[], query: string): SlashSuggestion[] {
  const terms = norm(query).split(/\s+/).filter(Boolean);
  if (terms.length === 0) return skills;
  return skills.filter((s) => {
    const hay = norm(`${s.token} ${s.skillName} ${s.summary}`);
    return terms.every((t) => hay.includes(t));
  });
}

/// スキルの検索欄（16px・検索キー）。
export function SkillSearchInput({
  value,
  onChange,
  inputRef,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  inputRef?: React.Ref<HTMLInputElement>;
  className?: string;
}) {
  return (
    <div className={cn("relative", className)}>
      <Search
        className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
        aria-hidden
      />
      <input
        ref={inputRef}
        type="search"
        enterKeyHint="search"
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        aria-label="スキルを検索"
        placeholder="スキルを検索"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        data-testid="skill-search"
        className={cn(
          "h-11 w-full rounded-xl border border-border bg-background pl-9 pr-3 text-base outline-none md:h-9 md:text-sm",
          "placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring",
        )}
      />
    </div>
  );
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
            /{skill.token}
          </span>
          <span className="shrink-0 truncate text-[11.5px] text-muted-foreground">{skill.skillName}</span>
        </span>
        <span className="mt-0.5 block line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">
          {skill.summary}
        </span>
      </span>
    </button>
  );
}

/// すべてのスキルの一覧（検索・スクロール）。
export function SkillBrowser({
  open,
  onOpenChange,
  skills,
  onPick,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  skills: SlashSuggestion[];
  onPick: (s: SlashSuggestion) => void;
}) {
  const [query, setQuery] = React.useState("");
  const inputRef = React.useRef<HTMLInputElement | null>(null);
  const ranked = useRankedSkills(skills, open);
  const hits = filterSkills(ranked, query);
  React.useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="flex flex-col gap-3 overflow-hidden sm:h-[min(36rem,calc(100dvh-2rem))] sm:max-w-lg max-sm:h-[92dvh]"
        data-testid="skill-browser"
        // タッチで開いた瞬間に検索欄へフォーカスするとキーボードが一覧を隠す。マウスの端末だけ。
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          if (window.matchMedia("(pointer: fine)").matches) inputRef.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>スキル</DialogTitle>
          <DialogDescription>
            選ぶと入力欄にコマンドが入ります。入力欄で「/」を打っても呼べます。
          </DialogDescription>
        </DialogHeader>
        <SkillSearchInput value={query} onChange={setQuery} inputRef={inputRef} className="shrink-0" />
        <span className="sr-only" aria-live="polite">
          {query ? `${hits.length} 件` : ""}
        </span>
        <div className="-mx-2 min-h-0 flex-1 overflow-y-auto overscroll-contain px-2">
          {hits.map((s) => (
            <SkillRow
              key={s.key}
              skill={s}
              testId="skill-browser-option"
              onPick={(picked) => {
                onPick(picked);
                onOpenChange(false);
              }}
            />
          ))}
          {hits.length === 0 ? (
            <p className="px-3 py-8 text-center text-sm text-muted-foreground">
              {query ? `「${query.trim()}」に一致するスキルはありません` : "使えるスキルはありません"}
            </p>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}
