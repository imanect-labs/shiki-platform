"use client";

/// step の実行タイムライン（Task 10.14）。
///
/// run 詳細の steps 概要を上から順に並べ、失敗 step を強調する。入出力の本体は
/// 概要に載せない（一覧を軽く保つ）ため、行を開いたときに step 詳細 API で
/// 遅延取得する。出力内のシークレットは保存時に自動マスク済み（engine.md §7）。

import * as React from "react";
import {
  AlertCircle,
  Check,
  ChevronRight,
  CircleDashed,
  Clock,
  Loader2,
  MinusCircle,
  RotateCcw,
  XCircle,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { getStep, type StepDetail, type StepOverview } from "@/lib/workflow-run-api";
import { nodeIcon } from "@/components/workflow/editor/icons";
import { formatDateTime, STEP_STATUS_LABELS } from "./status";

/// node_id → 表示情報（run にピンされた version の IR から引く）。
export type NodeInfo = { label: string; type: string };

const PORT_LABELS: Record<string, string> = {
  true: "はい",
  false: "いいえ",
  error: "エラー時",
  timeout: "時間切れ",
  default: "その他",
};

function statusIcon(status: string): React.ReactNode {
  switch (status) {
    case "succeeded":
      return <Check className="size-3.5 text-[color:var(--season-summer)]" aria-hidden />;
    case "failed":
      return <XCircle className="size-3.5 text-destructive" aria-hidden />;
    case "running":
      return <Loader2 className="size-3.5 animate-spin text-primary" aria-hidden />;
    case "waiting_timer":
    case "waiting_event":
    case "waiting_map":
      return <Clock className="size-3.5 text-[color:var(--season-autumn)]" aria-hidden />;
    case "skipped":
      return <MinusCircle className="size-3.5 text-muted-foreground" aria-hidden />;
    case "cancelled":
      return <MinusCircle className="size-3.5 text-muted-foreground" aria-hidden />;
    default:
      return <CircleDashed className="size-3.5 text-muted-foreground" aria-hidden />;
  }
}

function errorMessage(error: unknown): string | null {
  if (!error) return null;
  if (typeof error === "string") return error;
  const e = error as { message?: string; code?: string };
  if (e.message) return e.code ? `${e.message}（${e.code}）` : e.message;
  return JSON.stringify(error);
}

function JsonBlock({ title, value }: { title: string; value: unknown }) {
  return (
    <div>
      <p className="mb-1 text-[11px] font-medium text-muted-foreground">{title}</p>
      <pre className="max-h-64 overflow-auto rounded-md border bg-muted/40 p-2 font-mono text-[11px] leading-relaxed scrollbar-subtle">
        {JSON.stringify(value, null, 2)}
      </pre>
    </div>
  );
}

/// 開いたときだけ step 詳細（出力・エラー全文）を取りに行く行。
function StepRow({
  workflowId,
  runId,
  step,
  info,
}: {
  workflowId: string;
  runId: string;
  step: StepOverview;
  info: NodeInfo | null;
}) {
  const [open, setOpen] = React.useState(false);
  const [detail, setDetail] = React.useState<StepDetail | "loading" | "error" | null>(
    null,
  );
  const failed = step.status === "failed";
  const running = step.status === "running";
  const message = errorMessage(step.error);
  const expandable = step.hasOutput || step.error !== null;
  const Icon = info ? nodeIcon(info.type) : null;

  React.useEffect(() => {
    if (!open || detail !== null) return;
    setDetail("loading");
    getStep(workflowId, runId, step.stepPath)
      .then(setDetail)
      .catch(() => setDetail("error"));
  }, [open, detail, workflowId, runId, step.stepPath]);

  return (
    <li
      className={cn(
        "relative rounded-lg border transition-colors duration-fast",
        failed
          ? "border-destructive/40 bg-destructive/5"
          : "bg-card",
        // 実行中は枠を光弧が時計回りに走る（進捗表示）。
        running && "shiki-running-border border-primary/30",
      )}
    >
      <button
        type="button"
        onClick={() => expandable && setOpen((o) => !o)}
        disabled={!expandable}
        className={cn(
          "flex w-full items-center gap-2.5 px-3 py-2.5 text-left",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-lg",
          expandable && "cursor-pointer",
        )}
      >
        <span className="flex size-6 shrink-0 items-center justify-center rounded-full border bg-background">
          {statusIcon(step.status)}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-1.5 text-xs font-medium">
            {Icon ? <Icon className="size-3.5 text-muted-foreground" aria-hidden /> : null}
            <span className="truncate">{info?.label ?? step.nodeId}</span>
            {step.attempt > 1 ? (
              <Badge variant="warning" className="px-1.5 py-0 text-[10px]">
                <RotateCcw className="size-2.5" aria-hidden />
                {step.attempt} 回目
              </Badge>
            ) : null}
            {step.takenPorts
              // 既定の成功ポートはノイズなので分岐系ポートだけ見せる。
              .filter((p) => p !== "next" && p !== "out")
              .map((p) => (
                <Badge key={p} variant="muted" className="px-1.5 py-0 text-[10px]">
                  {PORT_LABELS[p] ?? p}
                </Badge>
              ))}
          </span>
          <span className="mt-0.5 flex items-center gap-2 text-[11px] text-muted-foreground">
            <span>{STEP_STATUS_LABELS[step.status] ?? step.status}</span>
            {step.stepPath !== step.nodeId ? (
              <span className="truncate font-mono">{step.stepPath}</span>
            ) : null}
            {step.wakeAt && step.status.startsWith("waiting") ? (
              <span>{formatDateTime(step.wakeAt)} に再開予定</span>
            ) : null}
            {/* next_retry_at は実行リースにも使われるため、待ち状態のときだけ意味を持つ */}
            {step.nextRetryAt &&
            (step.status === "pending" || step.status === "ready") ? (
              <span>{formatDateTime(step.nextRetryAt)} に再試行</span>
            ) : null}
          </span>
          {failed && message ? (
            <span className="mt-1 flex items-start gap-1 text-[11px] text-destructive">
              <AlertCircle className="mt-0.5 size-3 shrink-0" aria-hidden />
              <span className="min-w-0 break-all">{message}</span>
            </span>
          ) : null}
        </span>
        {expandable ? (
          <ChevronRight
            className={cn(
              "size-4 shrink-0 text-muted-foreground transition-transform duration-fast",
              open && "rotate-90",
            )}
            aria-hidden
          />
        ) : null}
      </button>
      {open ? (
        <div className="space-y-2 border-t px-3 py-2.5">
          {detail === "loading" ? (
            <p className="flex items-center gap-2 text-[11px] text-muted-foreground">
              <Loader2 className="size-3 animate-spin" aria-hidden />
              読み込み中…
            </p>
          ) : detail === "error" ? (
            <p className="text-[11px] text-muted-foreground">詳細を取得できませんでした</p>
          ) : detail && detail !== null ? (
            <>
              {detail.error != null ? (
                <JsonBlock title="エラーの詳細" value={detail.error} />
              ) : null}
              {detail.output !== null && detail.output !== undefined ? (
                <JsonBlock title="このブロックの結果" value={detail.output} />
              ) : null}
              <p className="text-[10px] text-muted-foreground">
                シークレット（API キー等）は自動でマスクされています
                {detail.langfuseTraceId ? (
                  <span className="ml-2 font-mono">trace: {detail.langfuseTraceId}</span>
                ) : null}
              </p>
            </>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

export function StepTimeline({
  workflowId,
  runId,
  steps,
  nodeInfoOf,
}: {
  workflowId: string;
  runId: string;
  steps: StepOverview[];
  nodeInfoOf: (nodeId: string) => NodeInfo | null;
}) {
  if (steps.length === 0) {
    return (
      <p className="py-4 text-center text-xs text-muted-foreground">
        まだ実行されたブロックはありません
      </p>
    );
  }
  return (
    <ul className="space-y-1.5">
      {steps.map((s) => (
        <StepRow
          // 同じ workflow の別 run は step_path が一致するため、run 跨ぎの state 再利用を防ぐ。
          key={`${runId}:${s.stepPath}`}
          workflowId={workflowId}
          runId={runId}
          step={s}
          info={nodeInfoOf(s.nodeId)}
        />
      ))}
    </ul>
  );
}
