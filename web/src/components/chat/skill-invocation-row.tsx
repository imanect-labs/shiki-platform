"use client";

/// 回答の中で読み込んだスキル（`skill` ツールの呼び出し）を、他の操作のまとまりから分けて 1 行で出す。
///
/// スキルは「この回答がどの手順書に沿って動いたか」を決めるもので、検索や読み取りと同じ 1 件として
/// 「N 件の操作」に畳まれると、使ったこと自体が見えなくなる。行を押すと、読み込んだ内容（モデルに
/// 渡った指示文）をその場で開く。
///
/// 中身はスキルの作者が書いた文章なので、Markdown として描かず（リンク・画像・```ui を動かさない）
/// 文字のまま出す。共有された会話を実行者以外が見るときは、サーバがツール結果の本文を伏せる
/// （`redact_tool_results_for_viewer`）ので、そのときは見られない旨だけを出す。

import * as React from "react";
import { AlertTriangle, ChevronDown, Loader2, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { seasonIndexFor } from "@/lib/tool-display";
import { useRememberedOpen } from "./disclosure-memory";
import type { ToolActivityItem } from "./tool-activity";

/// skill ツールの呼び出しか（スキルの検索 `skill_search` は通常の操作として残す）。
export function isSkillInvocation(item: Pick<ToolActivityItem, "name">): boolean {
  return item.name === "skill";
}

/// 呼び出しの入力からスキル名を取り出す。
function skillName(input: unknown): string | null {
  if (typeof input !== "object" || input === null) return null;
  const name = (input as { name?: unknown }).name;
  return typeof name === "string" && name.trim() ? name.trim() : null;
}

/// 読み込んだ内容の先頭の見出し（サーバが付ける `# Skill: <名前>`）を落とす。名前は行に出している。
function instructionsOf(result: string): string {
  return result.replace(/^# Skill: [^\n]*\n+/, "").trim();
}

export function SkillInvocationRow({
  item,
  memoryKey = null,
}: {
  item: ToolActivityItem;
  /// 開閉を覚えておく鍵。確定で行が作り直されても開閉を保つ。
  memoryKey?: string | null;
}) {
  const [open, setOpen] = useRememberedOpen(memoryKey);
  const panelId = React.useId();
  const name = skillName(item.input);
  const failed = item.ok === false;
  const label = name ? `スキル「${name}」` : "スキル";
  const status = item.running
    ? `${label}を読み込んでいます…`
    : failed
      ? `${label}を読み込めませんでした`
      : `${label}を読み込みました`;
  const body = item.result ? instructionsOf(item.result) : "";
  // 実行中は中身がまだ無いので開けない。
  const expandable = !item.running;

  // 色と並びは隣の操作の行（ToolActivity の見出し）に揃える: 状態の印 → 文言 → 版 → 右端の開閉。
  const season = seasonVar(seasonIndexFor("meta", item.running));

  return (
    <div className="mb-2.5" data-testid="skill-invocation">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        disabled={!expandable}
        aria-expanded={expandable ? open : undefined}
        aria-controls={expandable ? panelId : undefined}
        className={cn(
          "group flex w-full items-center gap-2 rounded-lg py-1 text-left text-[13px] transition-colors",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          expandable ? "hover:bg-accent/30" : "cursor-default",
        )}
      >
        {item.running ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin" style={{ color: season }} aria-hidden />
        ) : failed ? (
          <AlertTriangle className="size-3.5 shrink-0 text-destructive" aria-hidden />
        ) : (
          <Sparkles className="size-3.5 shrink-0" style={{ color: season }} aria-hidden />
        )}
        <span
          className={cn(
            "min-w-0 flex-1 truncate font-medium",
            item.running ? "shiki-text-shimmer" : failed ? "text-destructive" : "text-foreground/85",
          )}
          aria-live="polite"
        >
          {status}
        </span>
        {item.skillVersion !== undefined ? (
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">v{item.skillVersion}</span>
        ) : null}
        {expandable ? (
          <ChevronDown
            className={cn(
              "size-3.5 shrink-0 text-muted-foreground transition-transform",
              "duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
              open && "rotate-180",
            )}
            aria-hidden
          />
        ) : null}
      </button>

      {expandable && open ? (
        // 中身の左端は見出しの文言に揃える（印の幅＋間隔・ToolActivity の展開と同じ字下げ）。
        <div id={panelId} className="mt-1 pl-[1.375rem]">
          {body ? (
            // 作者が書いた指示文の原文。整形せず、原文と分かる等幅で出す（リンク・画像・```ui を動かさない）。
            <pre
              className={cn(
                "max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border/60 bg-card/40 p-3",
                "font-mono text-[12px] leading-relaxed",
                failed ? "text-destructive" : "text-foreground/80",
              )}
              data-testid="skill-invocation-content"
            >
              {body}
            </pre>
          ) : (
            <p className="text-[12px] text-muted-foreground">
              読み込んだ内容は、この回答を実行した人だけが見られます。
            </p>
          )}
        </div>
      ) : null}
    </div>
  );
}
