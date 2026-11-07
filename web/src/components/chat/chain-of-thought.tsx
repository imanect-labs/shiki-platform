// 思考プロセス（Chain of Thought）の表示。生の reasoning をそのまま見せると冗長で読みにくい
// ため、ツール実行は ToolActivity（フェーズ行＋ローリング＋インライン展開・#386）に委ね、
// ここは「詳細な思考テキスト」を受け持つ。参照ドキュメントは ToolActivity のチップへ移動。
"use client";

import * as React from "react";
import { Brain, ChevronDown, ChevronRight } from "lucide-react";

import { usePreferences } from "@/lib/preferences";
import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Citation } from "@/lib/chat-api";
import { useRememberedOpen } from "./disclosure-memory";
import { ToolActivity, type ToolActivityItem } from "./tool-activity";

export function ChainOfThought({
  thinking,
  tools,
  citations,
  streaming = false,
  /// 計画の進行中サブタスク（自律 run）。あればフェーズ行に優先して出す。
  phase = null,
  /// 次のイベントを待っている間の文言（待った時間で進む）。待っていなければ null。
  waitingLabel = null,
  memoryKey = null,
}: {
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  streaming?: boolean;
  phase?: string | null;
  waitingLabel?: string | null;
  /// 開閉を覚えておく鍵（回答 ID ＋まとまりの位置）。確定で行が作り直されても開閉を保つ。
  memoryKey?: string | null;
}) {
  // 押して開閉するまでは設定の既定（思考過程を開いて表示・#952）に従う。押した後の開閉は、確定で
  // 行が作り直されても保つ。
  const [open, setOpen] = useRememberedOpen(memoryKey && `${memoryKey}:thinking`, null);
  const expandByDefault = usePreferences().prefs.expand_reasoning;
  const [showDetail, setShowDetail] = useRememberedOpen(memoryKey && `${memoryKey}:detail`);

  const hasThinking = thinking.trim().length > 0;
  const hasTools = tools.length > 0;
  // citations は ToolActivity のチップに移した。hasContent から漏れるとチップ行ごと消える。
  const hasContent = hasThinking || hasTools || citations.length > 0;
  if (!hasContent && !streaming) return null;

  // 思考テキストのパネルは押したとき（または設定で最初から）開く。生成中だけ自動で開くと、
  // まとまりが終わった瞬間に閉じて行が縮む（パカパカする）ので、生成の状態では開閉しない。
  const expanded = open ?? expandByDefault;
  // ツールがあるまとまりでは、生きている表示（回転・待ちの文言）はツールの見出しが受け持つ。
  // 思考の見出しにも同じ文言を出すと 2 行が同じことを言う。
  const live = streaming && !hasTools;
  const status = statusText(live, thinking, waitingLabel);
  // 完了後は冬で固定し、生成中だけ春（準備・思考）で控えめに彩る。
  const stageSeason = seasonVar(live ? 0 : 3);

  return (
    <div className="mb-2.5">
      {/* 思考 → ツールの順（起きた順）。ツールが増えても思考の見出しは動かない。
          本文も操作もまだ無い生成の開始直後は「準備しています」を出す。 */}
      {hasThinking || (streaming && !hasTools && citations.length === 0) ? (
        <div className={cn(hasTools && "mb-1")}>
          <button
            type="button"
            onClick={() => setOpen(!expanded)}
            className="flex flex-wrap items-center gap-1.5 rounded-md py-0.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground"
            aria-expanded={expanded}
          >
            <Brain
              className={cn("size-3.5 transition-colors", live && "animate-pulse")}
              style={{ color: stageSeason }}
              aria-hidden
            />
            {/* 文言の変化を読み上げる領域は差し替えない（key を付けると領域ごと作り直されて
                読み上げられない）。入れ替えは中、脈打ちはさらに内側（同じ要素だと animation が
                上書きされ、片方しか効かない）。 */}
            <span aria-live="polite">
              <span key={status} className={cn("block", live && "animate-in fade-in-0 duration-300")}>
                <span className={cn("font-medium", live && "animate-pulse")}>{status}</span>
              </span>
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
                    onClick={() => setShowDetail(!showDetail)}
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
        </div>
      ) : null}

      {/* ツール実行（フェーズ行＋ローリング＋展開）。生成中でなくても履歴として残す。
          citations はフェーズ行のチップとして表示するため渡す（展開セクションは廃止）。 */}
      <ToolActivity
        items={tools}
        streaming={streaming}
        phaseOverride={phase}
        citations={citations}
        waitingLabel={waitingLabel}
        memoryKey={memoryKey && `${memoryKey}:tools`}
      />
    </div>
  );
}

/// 思考パネルの見出し。ツールの進行状況は ToolActivity のフェーズ行が受け持つので、
/// ここは「考えている / 思考プロセス」だけを担う（旧実装のようにツール文言へ依存しない）。
/// 思考が流れていない間に待たされているなら、待った時間に応じた文言（`waitingLabel`）を出す。
function statusText(live: boolean, thinking: string, waitingLabel: string | null): string {
  if (!live) return "思考プロセス";
  // 待ちの文言（思考が流れている間は「考えています」）を優先する。思考の後に長く止まる間も
  // 「考えています」のままにしない。
  if (waitingLabel) return `${waitingLabel}…`;
  return thinking.trim() ? "考えています…" : "準備しています…";
}
