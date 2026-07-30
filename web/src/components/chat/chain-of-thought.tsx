// 思考プロセス（Chain of Thought）の表示。生の reasoning をそのまま見せると冗長で読みにくい
// ため、ツール実行は ToolActivity（フェーズ行＋ローリング＋インライン展開・#386）に委ね、
// ここは「参照ドキュメント」と「詳細な思考テキスト」を受け持つ。
"use client";

import * as React from "react";
import { Brain, ChevronDown, ChevronRight, FileText } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Citation } from "@/lib/chat-api";
import { ToolActivity, type ToolActivityItem } from "./tool-activity";

function citationLabel(c: Citation): string {
  return c.heading_path && c.heading_path.length > 0
    ? c.heading_path[c.heading_path.length - 1]
    : "ドキュメント";
}

export function ChainOfThought({
  thinking,
  tools,
  citations,
  streaming = false,
  /// 計画の進行中サブタスク（自律 run）。あればフェーズ行に優先して出す。
  phase = null,
}: {
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  streaming?: boolean;
  phase?: string | null;
}) {
  const [open, setOpen] = React.useState(false);
  const [userToggled, setUserToggled] = React.useState(false);
  const [showDetail, setShowDetail] = React.useState(false);

  const hasThinking = thinking.trim().length > 0;
  const hasSide = hasThinking || citations.length > 0;
  const hasContent = hasSide || tools.length > 0;
  if (!hasContent && !streaming) return null;

  // 思考テキスト/引用のパネル。生成中は自動で開き、ユーザーが触ったらその意思を優先する。
  const expanded = userToggled ? open : streaming || open;
  const status = statusText(streaming, thinking, tools.length > 0);
  // 完了後は冬で固定し、生成中だけ春（準備・思考）で控えめに彩る。
  const stageSeason = seasonVar(streaming ? 0 : 3);

  return (
    <div className="mb-2.5">
      {/* ツール実行（フェーズ行＋ローリング＋展開）。生成中でなくても履歴として残す。 */}
      <ToolActivity items={tools} streaming={streaming} phaseOverride={phase} />

      {hasSide || streaming ? (
        <>
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
            {citations.length > 0 ? <Badge seasonIndex={2}>{`参照 ${citations.length}`}</Badge> : null}
            <ChevronRight
              className={cn("size-3.5 transition-transform", expanded && "rotate-90")}
              aria-hidden
            />
          </button>

          {expanded ? (
            <div className="mt-2 space-y-3 border-l-2 border-border pl-3">
              {/* 参照したドキュメント（番号は本文の [n] と一致）。 */}
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
                            <span className="ml-1 text-muted-foreground/90 line-clamp-1">
                              {c.snippet}
                            </span>
                          ) : null}
                        </span>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}

              {/* 詳細な思考（生の reasoning）は読みたい人だけ開く副次トグル */}
              {hasThinking ? (
                <div>
                  <button
                    type="button"
                    onClick={() => setShowDetail((v) => !v)}
                    className="flex items-center gap-1 text-[12px] text-muted-foreground/80 transition-colors hover:text-foreground"
                    aria-expanded={showDetail}
                  >
                    <ChevronDown
                      className={cn("size-3.5 transition-transform", showDetail && "rotate-180")}
                      aria-hidden
                    />
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
        </>
      ) : null}
    </div>
  );
}

/// 思考パネルの見出し。ツールの進行状況は ToolActivity のフェーズ行が受け持つので、
/// ここは「考えている / 思考プロセス」だけを担う（旧実装のようにツール文言へ依存しない）。
function statusText(streaming: boolean, thinking: string, hasTools: boolean): string {
  if (!streaming) return "思考プロセス";
  if (thinking.trim()) return "考えています…";
  return hasTools ? "回答をまとめています…" : "準備しています…";
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
