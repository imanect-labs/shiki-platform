"use client";

/// 実行履歴ページ（Task 10.14）: data-table ＋ ?run= deep-link の詳細シート。

import * as React from "react";
import { useParams, useRouter, useSearchParams } from "next/navigation";
import { Loader2 } from "lucide-react";

import { usePageChrome } from "@/components/shell/page-header-context";
import { RunDetailSheet } from "@/components/workflow/runs/run-detail-sheet";
import { RunsTable } from "@/components/workflow/runs/runs-table";
import { WorkflowTabs } from "@/components/workflow/workflow-tabs";
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

  // 戻る（エディタへ）はシェルのクローム（PC はバー左・スマホは左上）に置く。
  usePageChrome(
    () => ({
      title: "実行履歴",
      subtitle: title || undefined,
      back: { href: `/workflows/${id}`, label: "エディタへ戻る" },
    }),
    [id, title],
  );

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
    <div className="mx-auto w-full max-w-5xl px-4 pb-6 pt-1 md:py-6">
      <div className="mb-4 flex flex-col gap-3 md:mb-5">
        <div className="min-w-0">
          <p className="truncate text-[12.5px] text-muted-foreground md:hidden">
            {title === null ? "…" : title || "ワークフロー"}
          </p>
          <h1 className="text-[26px] font-bold leading-tight tracking-tight md:text-lg md:font-semibold">
            実行履歴
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">
            <span className="hidden md:inline">{title === null ? "…" : title || "ワークフロー"}の</span>
            実行の記録です。行を選ぶと詳細が開きます。
          </p>
        </div>
        <WorkflowTabs workflowId={id} current="runs" />
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
