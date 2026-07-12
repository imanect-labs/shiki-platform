"use client";

/// run 詳細シート（Task 10.14・?run= deep-link で開く）。
///
/// SSE で自動更新しながら、run 概要・step タイムライン・中止/再実行の操作を出す。
/// 中止は step 境界で検知される（実行中のブロックの即時中断はしない）ことを
/// 過約束せず明記する。node_id の表示名は run にピンされた version の IR から引く。

import * as React from "react";
import { Ban, Loader2, Play, RotateCcw } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetBody,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { toast } from "@/components/ui/use-toast";
import { getWorkflowVersion } from "@/lib/workflow-api";
import { cancelRun, retryRun } from "@/lib/workflow-run-api";
import type { WorkflowIr } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import { StepTimeline, type NodeInfo } from "./step-timeline";
import {
  FAIL_REASON_LABELS,
  formatDateTime,
  formatDuration,
  isTerminalRunStatus,
  RunStatusBadge,
  TRIGGER_KIND_LABELS,
} from "./status";
import { useRunStream } from "./use-run-stream";

const CATALOG_LABELS = new Map<string, string>(
  NODE_CATALOG.map((e) => [e.type, e.label_ja]),
);

/// run の version にピンされた IR の node_id → 表示情報。workflow×version でキャッシュする
/// （version 番号は workflow 間で衝突するため、単独キーだと別 workflow のラベルが残る）。
function useNodeInfo(workflowId: string, version: number | null) {
  const [map, setMap] = React.useState<Map<string, NodeInfo> | null>(null);
  const loadedKey = React.useRef<string | null>(null);

  React.useEffect(() => {
    if (version === null) return;
    const key = `${workflowId}:${version}`;
    if (loadedKey.current === key) return;
    loadedKey.current = key;
    setMap(null);
    let stale = false;
    getWorkflowVersion(workflowId, version)
      .then(({ ir }) => {
        if (stale) return;
        const nodes = (ir as WorkflowIr).nodes ?? [];
        setMap(
          new Map(
            nodes.map((n) => [
              n.id,
              { label: n.label || CATALOG_LABELS.get(n.type) || n.id, type: n.type },
            ]),
          ),
        );
      })
      .catch(() => {
        // IR が引けなくても node_id 表示で成立する。
        if (!stale) setMap(new Map());
      });
    return () => {
      stale = true;
    };
  }, [workflowId, version]);

  return React.useCallback(
    (nodeId: string): NodeInfo | null => map?.get(nodeId) ?? null,
    [map],
  );
}

function MetaItem({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div>
      <dt className="text-[11px] text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 text-xs font-medium">{value}</dd>
    </div>
  );
}

export function RunDetailSheet({
  workflowId,
  runId,
  onClose,
  onNavigateRun,
  onChanged,
}: {
  workflowId: string;
  runId: string | null;
  onClose: () => void;
  /// retry(new) で新しい run へ deep-link を張り替える。
  onNavigateRun: (runId: string) => void;
  /// cancel/retry 後に一覧を更新させる。
  onChanged: () => void;
}) {
  const { detail, error, refresh } = useRunStream(workflowId, runId);
  const nodeInfoOf = useNodeInfo(workflowId, detail?.version ?? null);
  const [busy, setBusy] = React.useState<"cancel" | "retry" | null>(null);

  // terminal への遷移を一覧へも反映する（SSE は詳細側にしか張っていない）。
  const prevStatus = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!detail) return;
    if (
      prevStatus.current !== null &&
      prevStatus.current !== detail.status &&
      isTerminalRunStatus(detail.status)
    ) {
      onChanged();
    }
    prevStatus.current = detail.status;
  }, [detail, onChanged]);

  const cancel = async () => {
    if (!runId) return;
    setBusy("cancel");
    try {
      const outcome = await cancelRun(workflowId, runId);
      toast({
        title:
          outcome === "requested"
            ? "中止をリクエストしました"
            : "すでに終了しています",
        description:
          outcome === "requested"
            ? "実行中のブロックが区切りに達したところで止まります。"
            : undefined,
      });
      refresh();
      onChanged();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "中止できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(null);
    }
  };

  const retry = async (mode: "resume" | "new") => {
    if (!runId) return;
    setBusy("retry");
    try {
      const newRunId = await retryRun(workflowId, runId, mode);
      onChanged();
      if (mode === "resume") {
        toast({ title: "失敗したところから再開しました" });
        refresh();
      } else if (newRunId) {
        toast({ title: "同じ入力でもう一度実行しました" });
        onNavigateRun(newRunId);
      } else {
        toast({
          title: "実行は受け付けられませんでした",
          description: "同時実行の上限（skip 設定）の可能性があります。",
        });
      }
    } catch (e) {
      toast({
        variant: "destructive",
        title: "再実行できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(null);
    }
  };

  const cancellable =
    detail !== null &&
    !isTerminalRunStatus(detail.status) &&
    !detail.cancelRequested;

  return (
    <Sheet open={runId !== null} onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="right" className="max-w-2xl">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            実行の詳細
            {detail ? <RunStatusBadge status={detail.status} /> : null}
            {detail?.cancelRequested && !isTerminalRunStatus(detail.status) ? (
              <span className="text-xs font-normal text-muted-foreground">
                中止しています…
              </span>
            ) : null}
          </SheetTitle>
          <SheetDescription className="font-mono text-[11px]">
            {runId ?? ""}
          </SheetDescription>
        </SheetHeader>
        <SheetBody>
          {error ? (
            <p className="py-8 text-center text-sm text-muted-foreground">{error}</p>
          ) : detail === null ? (
            <div className="flex items-center justify-center gap-2 py-12 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden />
              読み込み中…
            </div>
          ) : (
            <div className="space-y-5">
              {detail.status === "failed" && detail.failReason ? (
                <p className="rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs text-destructive">
                  {FAIL_REASON_LABELS[detail.failReason] ?? detail.failReason}
                </p>
              ) : null}

              <dl className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
                <MetaItem
                  label="きっかけ"
                  value={TRIGGER_KIND_LABELS[detail.triggerKind] ?? detail.triggerKind}
                />
                <MetaItem label="バージョン" value={`v${detail.version}`} />
                <MetaItem
                  label="所要時間"
                  value={formatDuration(detail.startedAt, detail.finishedAt)}
                />
                <MetaItem label="受付" value={formatDateTime(detail.createdAt)} />
                <MetaItem label="開始" value={formatDateTime(detail.startedAt)} />
                <MetaItem label="終了" value={formatDateTime(detail.finishedAt)} />
              </dl>

              <div className="flex flex-wrap items-center gap-2">
                {cancellable ? (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={cancel}
                    disabled={busy !== null}
                  >
                    {busy === "cancel" ? (
                      <Loader2 className="size-3.5 animate-spin" aria-hidden />
                    ) : (
                      <Ban className="size-3.5" aria-hidden />
                    )}
                    実行を中止
                  </Button>
                ) : null}
                {detail.status === "failed" ? (
                  <Button size="sm" onClick={() => retry("resume")} disabled={busy !== null}>
                    {busy === "retry" ? (
                      <Loader2 className="size-3.5 animate-spin" aria-hidden />
                    ) : (
                      <RotateCcw className="size-3.5" aria-hidden />
                    )}
                    失敗したところから再開
                  </Button>
                ) : null}
                {isTerminalRunStatus(detail.status) ? (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => retry("new")}
                    disabled={busy !== null}
                  >
                    <Play className="size-3.5" aria-hidden />
                    同じ入力でもう一度実行
                  </Button>
                ) : null}
              </div>

              {detail.input !== null && detail.input !== undefined ? (
                <div>
                  <h3 className="mb-1 text-[11px] font-medium text-muted-foreground">
                    実行時の入力
                  </h3>
                  <pre className="max-h-40 overflow-auto rounded-md border bg-muted/40 p-2 font-mono text-[11px] leading-relaxed scrollbar-subtle">
                    {JSON.stringify(detail.input, null, 2)}
                  </pre>
                </div>
              ) : null}

              <div>
                <h3 className="mb-1.5 text-[11px] font-medium text-muted-foreground">
                  ブロックごとの記録
                </h3>
                <StepTimeline
                  workflowId={workflowId}
                  runId={detail.runId}
                  steps={detail.steps}
                  nodeInfoOf={nodeInfoOf}
                />
              </div>
            </div>
          )}
        </SheetBody>
      </SheetContent>
    </Sheet>
  );
}
