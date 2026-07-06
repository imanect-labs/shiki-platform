// 思考プロセス（Chain of Thought）の表示。生の reasoning をそのまま見せると冗長で読みにくい
// ため、既定は「〜しています」という大雑把なステータス＋使用ツール（検索クエリ）＋参照ドキュメント
// を見せる。詳細な思考テキストは副次トグルの裏に隠す（読みたい人だけ開く）。
"use client";

import * as React from "react";
import { Brain, Check, ChevronDown, ChevronRight, FileText, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Citation } from "@/lib/chat-api";
import type { ToolActivityItem } from "./tool-activity";

/// ツール名 → 日本語の動作ラベル（Chain of Thought の可視化）。
const TOOL_VERB: Record<string, string> = {
  doc_search: "社内文書を検索",
  web_search: "web を検索",
  web_fetch: "ページを取得",
  code_interpreter: "コードを実行",
};

/// ツールの動作ラベル（未知ツールは名前をそのまま）。
function toolVerb(name: string): string {
  return TOOL_VERB[name] ?? name;
}

/// 進行段階を季節に対応づける（準備=春→考え中=春→検索=夏→まとめ=秋→完了=冬）。
/// 思考中だけ Brain アイコン/ステータスがゆっくり季節を移ろい、transient な彩りになる。
function stageSeasonIndex(status: string): number {
  if (status.startsWith("社内文書を検索")) return 1; // 夏
  if (status.startsWith("回答をまとめ")) return 2; // 秋
  if (status === "思考プロセス") return 3; // 冬（完了後）
  return 0; // 春（準備・考え中）
}

function toolQuery(input: unknown): string | null {
  if (input && typeof input === "object" && "query" in input) {
    const q = (input as { query?: unknown }).query;
    if (typeof q === "string" && q.trim()) return q.trim();
  }
  return null;
}

function citationLabel(c: Citation): string {
  return c.heading_path && c.heading_path.length > 0
    ? c.heading_path[c.heading_path.length - 1]
    : "ドキュメント";
}

/// 進行状況を「〜しています」の 1 文に大雑把化する。
function coarseStatus(streaming: boolean, thinking: string, tools: ToolActivityItem[]): string {
  if (!streaming) return "思考プロセス";
  // 実行中のツールがあれば、そのツールの動作名で状況を出す（doc_search 以外も正しく表示）。
  const running = tools.find((t) => t.running);
  if (running) return `${toolVerb(running.name)}しています…`;
  if (tools.length > 0) return "回答をまとめています…";
  if (thinking.trim()) return "考えています…";
  return "準備しています…";
}

export function ChainOfThought({
  thinking,
  tools,
  citations,
  streaming = false,
}: {
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  streaming?: boolean;
}) {
  const [open, setOpen] = React.useState(false);
  const [userToggled, setUserToggled] = React.useState(false);
  const [showDetail, setShowDetail] = React.useState(false);
  const expanded = userToggled ? open : streaming || open;

  const hasContent = thinking.trim().length > 0 || tools.length > 0 || citations.length > 0;
  if (!hasContent && !streaming) return null;

  const status = coarseStatus(streaming, thinking, tools);
  // 思考中はステータスの段階に合わせて季節を移ろわせ、完了後は落ち着いた冬で固定する。
  const stageSeason = seasonVar(streaming ? stageSeasonIndex(status) : 3);

  return (
    <div className="mb-2.5">
      <button
        type="button"
        onClick={() => {
          setUserToggled(true);
          setOpen((v) => !v);
        }}
        className="flex flex-wrap items-center gap-1.5 rounded-md py-0.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground"
        aria-expanded={expanded}
      >
        <Brain
          className={cn("size-3.5 transition-colors", streaming && "animate-pulse")}
          style={{ color: stageSeason }}
          aria-hidden
        />
        <span className={cn("font-medium", streaming && "animate-pulse")}>{status}</span>
        {tools.length > 0 ? <Badge seasonIndex={1}>{`検索 ${tools.length}`}</Badge> : null}
        {citations.length > 0 ? <Badge seasonIndex={2}>{`参照 ${citations.length}`}</Badge> : null}
        <ChevronRight
          className={cn("size-3.5 transition-transform", expanded && "rotate-90")}
          aria-hidden
        />
      </button>

      {expanded ? (
        <div className="mt-2 space-y-3 border-l-2 border-border pl-3">
          {/* 大雑把なステップ: 使用ツール */}
          {tools.length > 0 ? (
            <div className="space-y-1.5">
              {tools.map((t) => {
                const q = toolQuery(t.input);
                const verb = toolVerb(t.name);
                return (
                  <div key={t.id} className="flex items-start gap-2 text-[13px]">
                    {t.running ? (
                      <Loader2 className="mt-0.5 size-3.5 shrink-0 animate-spin text-muted-foreground" aria-hidden />
                    ) : (
                      <Check className="mt-0.5 size-3.5 shrink-0 text-primary" aria-hidden />
                    )}
                    <div className="min-w-0 flex-1">
                      <span className="text-foreground">
                        {verb}
                        {t.running ? "しています" : "しました"}
                      </span>
                      {q ? <span className="text-muted-foreground">：「{q}」</span> : null}
                    </div>
                  </div>
                );
              })}
            </div>
          ) : null}

          {/* 参照したドキュメント（番号は本文の [n] と一致）。ファイルプレビュー画面は
              本 PR ではスコープ外のため非遷移で表示のみ（後続 PR でビューアを配線予定）。 */}
          {citations.length > 0 ? (
            <div className="text-[13px]">
              <div className="mb-1.5 flex items-center gap-1.5 text-muted-foreground">
                <FileText className="size-3.5" aria-hidden />
                参照したドキュメント
              </div>
              <ul className="space-y-1">
                {citations.map((c, i) => (
                  <li key={c.chunk_id} className="flex items-start gap-2 py-0.5">
                    <span
                      style={{ ["--season" as string]: seasonVar(i) }}
                      className="mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full bg-[var(--season)]/15 text-[10px] font-semibold text-[var(--season)]"
                    >
                      {i + 1}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="font-medium text-foreground/90">{citationLabel(c)}</span>
                      {c.snippet ? (
                        <span className="ml-1 text-muted-foreground/90 line-clamp-1">{c.snippet}</span>
                      ) : null}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {/* 詳細な思考（生の reasoning）は読みたい人だけ開く副次トグル */}
          {thinking.trim() ? (
            <div>
              <button
                type="button"
                onClick={() => setShowDetail((v) => !v)}
                className="flex items-center gap-1 text-[12px] text-muted-foreground/80 transition-colors hover:text-foreground"
                aria-expanded={showDetail}
              >
                <ChevronDown className={cn("size-3.5 transition-transform", showDetail && "rotate-180")} aria-hidden />
                詳細な思考{showDetail ? "を隠す" : "を表示"}
              </button>
              {showDetail ? (
                <p className="mt-1.5 whitespace-pre-wrap rounded-md bg-muted/40 p-2.5 text-[12px] leading-relaxed text-muted-foreground">
                  {thinking}
                </p>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function Badge({ children, seasonIndex }: { children: React.ReactNode; seasonIndex?: number }) {
  if (seasonIndex == null) {
    return (
      <span className="rounded-full border border-border bg-card px-1.5 py-px text-[11px] text-muted-foreground">
        {children}
      </span>
    );
  }
  // 季節の差し色つきバッジ（枠/地はごく薄く、文字は季節色で控えめに主張する）。
  return (
    <span
      style={{ ["--season" as string]: seasonVar(seasonIndex) }}
      className="rounded-full border border-[var(--season)]/35 bg-[var(--season)]/[0.08] px-1.5 py-px text-[11px] font-medium text-[var(--season)]"
    >
      {children}
    </span>
  );
}
