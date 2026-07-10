"use client";

/// 実行履歴ページ（Task 10.14）: data-table ＋ ?run= deep-link の詳細シート。

import * as React from "react";
import { useParams, useRouter, useSearchParams } from "next/navigation";
import { ArrowLeft, Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { RunDetailSheet } from "@/components/workflow/runs/run-detail-sheet";
import { RunsTable } from "@/components/workflow/runs/runs-table";
import { getWorkflow } from "@/lib/workflow-api";

function RunsPageBody() {
  const params = useParams<{ id: string }>();
  const id = params.id;
  const router = useRouter();
  const searchParams = useSearchParams();
  const activeRunId = searchParams.get("run");

  const [title, setTitle] = React.useState<string | null>(null);
  const [refreshSignal, setRefreshSignal] = React.useState(0);

  React.useEffect(() => {
    getWorkflow(id)
      .then(({ ir }) => setTitle(ir.display_name || ir.name))
      .catch(() => setTitle(""));
  }, [id]);

  /// ?run= だけを書き換える（履歴を汚さない replace・スクロール維持）。
  const setRun = React.useCallback(
    (runId: string | null) => {
      const url = runId
        ? `/workflows/${id}/runs?run=${runId}`
        : `/workflows/${id}/runs`;
      router.replace(url, { scroll: false });
    },
    [router, id],
  );

  return (
    <div className="mx-auto w-full max-w-5xl px-4 py-6">
      <div className="mb-5 flex items-center gap-3">
        <Button
          variant="ghost"
          size="icon"
          aria-label="エディタへ戻る"
          onClick={() => router.push(`/workflows/${id}`)}
        >
          <ArrowLeft className="size-4" aria-hidden />
        </Button>
        <div>
          <h1 className="text-lg font-semibold">実行履歴</h1>
          <p className="text-sm text-muted-foreground">
            {title === null ? "…" : title || "ワークフロー"}
            の実行の記録です。行をクリックすると詳細が開きます。
          </p>
        </div>
      </div>

      <RunsTable
        workflowId={id}
        activeRunId={activeRunId}
        onOpenRun={(runId) => setRun(runId)}
        refreshSignal={refreshSignal}
      />

      <RunDetailSheet
        workflowId={id}
        runId={activeRunId}
        onClose={() => setRun(null)}
        onNavigateRun={(runId) => setRun(runId)}
        onChanged={() => setRefreshSignal((n) => n + 1)}
      />
    </div>
  );
}

export default function WorkflowRunsPage() {
  // useSearchParams はプリレンダ時に Suspense 境界が必須。
  return (
    <React.Suspense
      fallback={
        <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      }
    >
      <RunsPageBody />
    </React.Suspense>
  );
}
