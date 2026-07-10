"use client";

/// ワークフロー一覧ページ（Task 10.12/10.14）: 作成・エディタ/実行履歴への導線・有効化状態。

import * as React from "react";
import { useRouter } from "next/navigation";
import {
  AlertTriangle,
  CalendarClock,
  FileInput,
  Loader2,
  MousePointerClick,
  Plus,
  Workflow,
  Zap,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import {
  createWorkflow,
  listWorkflows,
  type WorkflowSummary,
} from "@/lib/workflow-api";
import type { WorkflowIr } from "@/generated/workflow-ir";

/// 新規作成時の空フロー（手動トリガのみ・エディタでノードを足していく）。
function emptyIr(name: string): WorkflowIr {
  return {
    ir_version: 1,
    name,
    display_name: "新しいワークフロー",
    description: null,
    declared_scopes: [],
    triggers: [{ kind: "interactive", label: null }],
    input_schema: null,
    nodes: [],
    edges: [],
    policies: {
      run_timeout_sec: 259200,
      max_parallel_runs: 10,
      on_trigger_overflow: "queue",
    },
  } as unknown as WorkflowIr;
}

const TRIGGER_LABELS: Record<string, { label: string; icon: React.ElementType }> = {
  schedule: { label: "スケジュール", icon: CalendarClock },
  event: { label: "イベント", icon: Zap },
  interactive: { label: "手動", icon: MousePointerClick },
};

function StatusBadge({ status }: { status: WorkflowSummary["enabledStatus"] }) {
  switch (status) {
    case "enabled":
      return <Badge variant="success">有効</Badge>;
    case "suspended_reconsent":
      return (
        <Badge variant="warning">
          <AlertTriangle className="size-3" aria-hidden />
          再同意が必要
        </Badge>
      );
    case "disabled":
      return <Badge variant="muted">無効</Badge>;
    default:
      return null;
  }
}

export default function WorkflowsPage() {
  const router = useRouter();
  const [items, setItems] = React.useState<WorkflowSummary[] | null>(null);
  const [creating, setCreating] = React.useState(false);

  React.useEffect(() => {
    listWorkflows()
      .then(setItems)
      .catch((e) => {
        setItems([]);
        toast({
          variant: "destructive",
          title: "一覧の取得に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        });
      });
  }, []);

  const create = async () => {
    setCreating(true);
    try {
      // name は tenant 内一意（安定参照名）。時刻ベースで衝突しない値を採番する。
      const name = `flow-${Date.now().toString(36)}`;
      const saved = await createWorkflow(emptyIr(name));
      router.push(`/workflows/${saved.id}`);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "作成に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
      setCreating(false);
    }
  };

  return (
    <div className="mx-auto w-full max-w-4xl px-4 py-6">
      <div className="mb-5 flex items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-semibold">ワークフロー</h1>
          <p className="text-sm text-muted-foreground">
            ブロックをつないで、定型作業を自動で動く流れにできます。
          </p>
        </div>
        <Button onClick={create} disabled={creating}>
          {creating ? (
            <Loader2 className="size-4 animate-spin" aria-hidden />
          ) : (
            <Plus className="size-4" aria-hidden />
          )}
          新しいワークフロー
        </Button>
      </div>

      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <EmptyState
          icon={Workflow}
          title="まだワークフローがありません"
          description="「新しいワークフロー」から作成すると、ドラッグ＆ドロップで流れを組み立てられます。"
        />
      ) : (
        <ul className="divide-y rounded-xl border bg-card">
          {items.map((wf) => (
            <li key={wf.id}>
              <button
                type="button"
                onClick={() => router.push(`/workflows/${wf.id}`)}
                className="flex w-full items-center gap-4 px-4 py-3.5 text-left transition-colors duration-fast hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
                  <Workflow className="size-4.5" aria-hidden />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium">
                      {wf.displayName || wf.name}
                    </span>
                    <StatusBadge status={wf.enabledStatus} />
                  </span>
                  <span className="mt-0.5 flex items-center gap-3 text-xs text-muted-foreground">
                    {wf.description ? (
                      <span className="truncate">{wf.description}</span>
                    ) : null}
                    <span className="flex shrink-0 items-center gap-2">
                      {[...new Set(wf.triggerKinds)].map((k) => {
                        const t = TRIGGER_LABELS[k];
                        if (!t) return null;
                        const Icon = t.icon;
                        return (
                          <span key={k} className="flex items-center gap-1">
                            <Icon className="size-3" aria-hidden />
                            {t.label}
                          </span>
                        );
                      })}
                    </span>
                  </span>
                </span>
                <span className="flex shrink-0 items-center gap-3 text-xs text-muted-foreground">
                  <span className="flex items-center gap-1">
                    <FileInput className="size-3" aria-hidden />v{wf.currentVersion}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
