// 思考プロセス（Chain of Thought）の表示。生の reasoning をそのまま見せると冗長で読みにくい
// ため、ツール実行は ToolActivity（フェーズ行＋ローリング＋インライン展開・#386）に委ね、
// ここは「詳細な思考テキスト」を受け持つ。参照ドキュメントは ToolActivity のチップへ移動。
"use client";

import * as React from "react";
import { Brain, ChevronDown, ChevronRight } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Citation } from "@/lib/chat-api";
import { ToolActivity, type ToolActivityItem } from "./tool-activity";

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
  // citations は ToolActivity のチップに移したので、ここでは思考テキストだけを担う。
  const hasSide = hasThinking;
  const hasContent = hasSide || tools.length > 0;
  if (!hasContent && !streaming) return null;

  // 思考テキスト/引用のパネル。生成中は自動で開き、ユーザーが触ったらその意思を優先する。
  const expanded = userToggled ? open : streaming || open;
  const status = statusText(streaming, thinking, tools.length > 0);
  // 完了後は冬で固定し、生成中だけ春（準備・思考）で控えめに彩る。
  const stageSeason = seasonVar(streaming ? 0 : 3);

  return (
    <div className="mb-2.5">
      {/* ツール実行（フェーズ行＋ローリング＋展開）。生成中でなくても履歴として残す。
          citations はフェーズ行のチップとして表示するため渡す（展開セクションは廃止）。 */}
      <ToolActivity items={tools} streaming={streaming} phaseOverride={phase} citations={citations} />

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
            <ChevronRight
              className={cn("size-3.5 transition-transform", expanded && "rotate-90")}
              aria-hidden
            />
          </button>

          {expanded ? (
            <div className="mt-2 space-y-3 border-l-2 border-border pl-3">
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

