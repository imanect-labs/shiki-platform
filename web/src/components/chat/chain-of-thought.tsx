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
  /// 最後のツールの後に回答の本文が流れ始めたか（ツールのローリングを畳む合図）。
  answering = false,
  /// 次のイベントを待っている間の文言（待った時間で進む）。待っていなければ null。
  waitingLabel = null,
  /// 回答の本文が出始めているか（思考もツールも無い応答では、見出しを本文の前だけ出す）。
  hasText = false,
}: {
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  streaming?: boolean;
  phase?: string | null;
  answering?: boolean;
  waitingLabel?: string | null;
  hasText?: boolean;
}) {
  const [open, setOpen] = React.useState(false);
  const [userToggled, setUserToggled] = React.useState(false);
  const [showDetail, setShowDetail] = React.useState(false);

  const hasThinking = thinking.trim().length > 0;
  // citations は ToolActivity のチップに移した。hasContent から漏れるとチップ行ごと消える。
  const hasSide = hasThinking;
  const hasContent = hasSide || tools.length > 0 || citations.length > 0;
  if (!hasContent && !streaming) return null;

  // 思考テキスト/引用のパネル。生成中は自動で開き、ユーザーが触ったらその意思を優先する。
  const expanded = userToggled ? open : streaming || open;
  const status = statusText(streaming, thinking, tools.length > 0, waitingLabel);
  // 完了後は冬で固定し、生成中だけ春（準備・思考）で控えめに彩る。
  const stageSeason = seasonVar(streaming ? 0 : 3);

  return (
    <div className="mb-2.5">
      {/* ツール実行（フェーズ行＋ローリング＋展開）。生成中でなくても履歴として残す。
          citations はフェーズ行のチップとして表示するため渡す（展開セクションは廃止）。 */}
      <ToolActivity
        items={tools}
        streaming={streaming}
        phaseOverride={phase}
        citations={citations}
        answering={answering}
        waitingLabel={waitingLabel}
      />

      {/* 思考もツールも無い応答では、本文が出る前の「準備しています」だけを出す。本文が出た後も
          残すと、本文の末尾の待ち表示（「続きを考えています」）と食い違う見出しが並ぶ。 */}
      {hasSide || (streaming && tools.length === 0 && citations.length === 0 && !hasText) ? (
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
            <span
              // 待ち時間の文言は数秒ごとに進むので、変わったら薄く入れ替える。入れ替えは外側、
              // 脈打ちは内側（同じ要素だと animation が上書きされ、片方しか効かない）。
              key={status}
              className={cn(streaming && "animate-in fade-in-0 duration-300")}
            >
              <span className={cn("font-medium", streaming && "animate-pulse")}>{status}</span>
            </span>
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
/// 思考が流れていない間に待たされているなら、待った時間に応じた文言（`waitingLabel`）を出す。
function statusText(
  streaming: boolean,
  thinking: string,
  hasTools: boolean,
  waitingLabel: string | null,
): string {
  if (!streaming) return "思考プロセス";
  // 待ちの文言（思考が流れている間は「考えています」）を優先する。思考の後に長く止まる間も
  // 「考えています」のままにしない。
  if (waitingLabel) return `${waitingLabel}…`;
  if (thinking.trim()) return "考えています…";
  return hasTools ? "回答をまとめています…" : "準備しています…";
}

