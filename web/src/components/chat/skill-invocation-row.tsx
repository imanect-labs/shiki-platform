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
import { AlertTriangle, ChevronRight, Loader2, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
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

  return (
    <div className="mb-2.5" data-testid="skill-invocation">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        disabled={!expandable}
        aria-expanded={expandable ? open : undefined}
        aria-controls={expandable ? panelId : undefined}
        className={cn(
          "flex max-w-full items-center gap-1.5 rounded-md py-0.5 text-left text-[13px] transition-colors",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          failed ? "text-destructive" : "text-muted-foreground hover:text-foreground",
          !expandable && "cursor-default hover:text-muted-foreground",
        )}
      >
        {item.running ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin" aria-hidden />
        ) : failed ? (
          <AlertTriangle className="size-3.5 shrink-0" aria-hidden />
        ) : (
          <Sparkles className="size-3.5 shrink-0 text-primary" aria-hidden />
        )}
        <span className="min-w-0 truncate font-medium" aria-live="polite">
          {status}
        </span>
        {item.skillVersion !== undefined ? (
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">v{item.skillVersion}</span>
        ) : null}
        {expandable ? (
          <ChevronRight
            className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")}
            aria-hidden
          />
        ) : null}
      </button>

      {expandable && open ? (
        <div id={panelId} className="mt-2 border-l-2 border-border pl-3">
          {body ? (
            <pre
              className={cn(
                "max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md p-2.5 font-sans text-[12px] leading-relaxed",
                failed ? "bg-destructive/5 text-destructive" : "bg-muted/40 text-muted-foreground",
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
