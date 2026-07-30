"use client";

/// 計画カード（実行前の計画提示＋開始 / 修正・#387）。
///
/// `plan` メタツールの計画パネルは**表示専用**でブロックしないため、「これで進めてよいか」を
/// 問う面がこれまで無かった。承認ゲート（Approver）はツール呼び出し単位の機構で計画には
/// 粒度が合わないので、genui のカードとして出し、押下を `chat.submit` の発話へ写す。
/// カードは generative_ui として永続するので、離席して戻っても承認導線が消えない。
///
/// **行の見た目は計画パネル（`chat/agent-progress.tsx` の `PlanStepRow`）と共有する**
/// （プラン UI を二重化しない）。

import * as React from "react";

import { ListChecks, PencilLine, Play } from "lucide-react";

import type { PlanCardProps } from "@/generated/gui-spec";
import { Button } from "@/components/ui/button";
import { PRESSABLE } from "@/components/ui/motion-primitives";
import { PlanStepRow } from "@/components/chat/agent-progress";
import { currentSeasonIndex, seasonAccentStyle } from "@/lib/season";
import { cn } from "@/lib/utils";
import { useGenUiAction } from "./action-context";
import { ActionResultNote, describeActionError } from "./action-result";

export function GenUiPlanCard({ card }: { card: PlanCardProps }) {
  const { dispatch, onActionCompleted } = useGenUiAction();
  const [revising, setRevising] = React.useState(false);
  const [revision, setRevision] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [done, setDone] = React.useState<"started" | "revised" | null>(null);
  const [error, setError] = React.useState<string | null>(null);

  const steps = card.steps ?? [];
  if (steps.length === 0) return null;

  const submit = async (payload: Record<string, string>, kind: "started" | "revised") => {
    if (busy || done) return;
    setBusy(true);
    setError(null);
    try {
      const result = await dispatch(card.submit.action, payload);
      setDone(kind);
      onActionCompleted?.(result);
    } catch (err) {
      setError(describeActionError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={seasonAccentStyle(currentSeasonIndex())} className="min-w-0">
      <div className="flex items-center gap-2.5">
        <span
          className="grid size-8 shrink-0 place-items-center rounded-lg"
          style={{
            backgroundColor: "color-mix(in oklab, var(--season) 16%, transparent)",
            color: "var(--season)",
          }}
          aria-hidden
        >
          <ListChecks className="size-4" />
        </span>
        <h3 className="min-w-0 flex-1 truncate text-sm font-semibold tracking-tight text-foreground">
          {card.title || "調査計画"}
        </h3>
        <span className="shrink-0 text-[11px] font-medium tabular-nums text-muted-foreground">
          {steps.length} ステップ
        </span>
      </div>

      {card.intro ? (
        <p className="mt-2 whitespace-pre-wrap text-[13px] leading-relaxed text-muted-foreground">
          {card.intro}
        </p>
      ) : null}

      <ol className="mt-3 space-y-1.5" data-testid="genui-plan-steps">
        {steps.map((s, i) => (
          <PlanStepRow key={`${i}-${s.title}`} title={s.title} description={s.description} />
        ))}
      </ol>

      {revising ? (
        <div className="mt-3">
          <textarea
            value={revision}
            onChange={(e) => setRevision(e.target.value)}
            placeholder="足したい視点や、外したい論点を書いてください"
            aria-label="計画の修正指示"
            rows={3}
            disabled={busy}
            className="w-full resize-y rounded-lg border border-input bg-background px-3 py-2 text-sm leading-relaxed text-foreground shadow-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
          />
        </div>
      ) : null}

      <div className="mt-3 flex items-center gap-2">
        {done ? (
          <span className="text-xs text-primary">
            {done === "started" ? "調査を開始しました" : "修正を送りました"}
          </span>
        ) : revising ? (
          <>
            <Button
              type="button"
              size="sm"
              disabled={busy || !revision.trim()}
              onClick={() => void submit({ 計画の修正: revision.trim() }, "revised")}
              className={PRESSABLE}
            >
              修正を送る
            </Button>
            <button
              type="button"
              onClick={() => setRevising(false)}
              className={cn(
                "rounded-lg px-2.5 py-1.5 text-xs font-medium text-muted-foreground",
                "transition-colors hover:bg-secondary hover:text-foreground",
                PRESSABLE,
              )}
            >
              やめる
            </button>
          </>
        ) : (
          <>
            <Button
              type="button"
              size="sm"
              disabled={busy}
              data-testid="genui-plan-start"
              onClick={() => void submit({ 計画の確認: card.submit_label || "この計画で開始" }, "started")}
              className={PRESSABLE}
            >
              <Play className="size-3.5" aria-hidden />
              {card.submit_label || "この計画で開始"}
            </Button>
            {card.allow_revise ? (
              <button
                type="button"
                onClick={() => setRevising(true)}
                className={cn(
                  "inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs font-medium text-muted-foreground",
                  "transition-colors hover:bg-secondary hover:text-foreground",
                  PRESSABLE,
                )}
              >
                <PencilLine className="size-3.5" aria-hidden />
                修正する
              </button>
            ) : null}
          </>
        )}
      </div>
      <ActionResultNote error={error} />
    </div>
  );
}
