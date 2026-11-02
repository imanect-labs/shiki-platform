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
  Trash2,
  Workflow,
  Zap,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { ListLoadFailed, ListMore } from "@/components/artifacts/shared-more";
import { PageContainer } from "@/components/shell/page-container";
import { useOwnedSharedList } from "@/hooks/use-owned-shared-list";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";
import { deleteArtifact } from "@/lib/artifact-api";
import {
  createWorkflow,
  listWorkflows,
  type WorkflowSummary,
} from "@/lib/workflow-api";
import type { PageRequest } from "@/lib/owned-shared-list";
import type { WorkflowIr } from "@/generated/workflow-ir";

const PAGE_SIZE = 50;

/// 一覧の 1 ページ（自分のものと共有されたものの続きは、それぞれの位置から読む・BUG-185）。
const workflowPages = (req: PageRequest) => listWorkflows({ ...req, limit: PAGE_SIZE });

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
  // 自分のものと共有されたものの続きを別の位置から読む。読み込みの失敗は空の状態に見せない（BUG-185・BUG-189）。
  const list = useOwnedSharedList(workflowPages);
  const { items } = list;
  const [creating, setCreating] = React.useState(false);
  const [deleting, setDeleting] = React.useState<WorkflowSummary | null>(null);

  const remove = async (wf: WorkflowSummary) => {
    try {
      // workflow は artifact（kind=workflow）なので共通のソフト削除で消す。
      await deleteArtifact(wf.id);
      list.remove(wf.id);
      toast({ title: `「${wf.displayName || wf.name}」を削除しました` });
    } catch (e) {
      toast({
        variant: "destructive",
        title: "削除できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setDeleting(null);
    }
  };

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

  const createButton = (
    <Button onClick={create} disabled={creating}>
      {creating ? (
        <Loader2 className="size-4 animate-spin" aria-hidden />
      ) : (
        <Plus className="size-4" aria-hidden />
      )}
      <span className="sm:hidden">新規作成</span>
      <span className="hidden sm:inline">新しいワークフロー</span>
    </Button>
  );

  return (
    <PageContainer
      title="ワークフロー"
      description="ブロックをつないで、定型作業を自動で動く流れにできます。"
      className="max-w-4xl"
      actions={createButton}
    >
      {items === null ? (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : list.error ? (
        <ListLoadFailed what="ワークフローの一覧" onRetry={list.reload} />
      ) : items.length === 0 && list.hasMore ? (
        <ListMore noun="ワークフロー" found={false} list={list} />
      ) : items.length === 0 ? (
        <EmptyState
          seasonal
          icon={Workflow}
          title="まだワークフローがありません"
          description="作成すると、ブロックを順に足して流れを組み立てられます。"
          action={
            <Button variant="outline" onClick={create} disabled={creating}>
              <Plus className="size-4" aria-hidden />
              ワークフローを作成
            </Button>
          }
        />
      ) : (
        <ul className="rule-soft overflow-hidden rounded-xl border bg-card">
          {items.map((wf, i) => (
            <li
              key={wf.id}
              data-testid="workflow-row"
              className={cn(
                "group/row relative flex items-center",
                i > 0 && "shiki-dash-top",
              )}
            >
              <button
                type="button"
                onClick={() => router.push(`/workflows/${wf.id}`)}
                className="flex min-w-0 flex-1 items-center gap-4 px-4 py-3.5 text-left transition-colors duration-fast hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
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
              <Button
                variant="ghost"
                size="icon"
                aria-label={`「${wf.displayName || wf.name}」を削除`}
                // マウスではホバーで出す。タッチ端末には hover が無いので常に出す。
                className="mr-2 shrink-0 text-muted-foreground transition-opacity duration-fast hover:text-destructive focus-visible:opacity-100 pointer-fine:opacity-0 pointer-fine:group-hover/row:opacity-100"
                onClick={() => setDeleting(wf)}
              >
                <Trash2 className="size-4" aria-hidden />
              </Button>
            </li>
          ))}
        </ul>
      )}
      {items !== null && items.length > 0 ? <ListMore noun="ワークフロー" found list={list} /> : null}

      <Dialog open={deleting !== null} onOpenChange={(o) => !o && setDeleting(null)}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>ワークフローを削除しますか？</DialogTitle>
            {/* 影響の範囲と戻せる度合い（付録 C の §3.3・論理削除）。動いている実行（承認待ちを含む）も止め、承認者の
                「承認」の画面から依頼を取り下げる（ほかの人に及ぶ影響・BUG-128）。 */}
            <DialogDescription>
              「{deleting ? deleting.displayName || deleting.name : ""}」を削除すると、自動実行も、動いている実行も止まり、承認者に出している依頼も取り下げられます。元に戻せません。データは消さずに保管されます。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleting(null)}>
              キャンセル
            </Button>
            <Button variant="destructive" onClick={() => deleting && void remove(deleting)}>
              削除する
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}
