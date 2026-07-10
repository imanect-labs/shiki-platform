"use client";

/// 有効化（同意）ダイアログ（Task 10.4a/10.12・engine.md §10）。
///
/// スケジュール/イベントで自動実行するには、**自分の権限の範囲から**ワークフローに
/// 権限を渡す（委譲）必要がある。consent-plan（IR 静的分析）の提案を日本語で見せ、
/// 対象未確定のものはフォルダピッカーで選ばせてから enable する。

import * as React from "react";
import { AlertTriangle, FolderOpen, Loader2, ShieldCheck } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import {
  disableWorkflow,
  enableWorkflow,
  getConsentPlan,
  type GrantInput,
  type Registration,
  type SuggestedGrant,
} from "@/lib/workflow-api";

/// スコープの日本語説明（権限の天井として同意させる内容）。
const SCOPE_LABELS: Record<string, string> = {
  "storage.read": "ファイルを読む",
  "storage.write": "ファイルを保存する",
  "rag.query": "社内ドキュメントを検索する",
  "http.egress": "外部サービスへ送信する",
  "workflow.start": "別のワークフローを起動する",
};

const RELATION_LABELS: Record<string, string> = {
  viewer: "閲覧",
  editor: "編集",
  can_use: "使用",
};

type GrantRow = SuggestedGrant & { picked?: { id: string; name: string } };

export function EnableDialog({
  open,
  onOpenChange,
  workflowId,
  version,
  registration,
  onChanged,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workflowId: string;
  version: number;
  registration: Registration | null;
  onChanged: () => void;
}) {
  const [plan, setPlan] = React.useState<{
    declaredScopes: string[];
    grants: GrantRow[];
  } | null>(null);
  const [pickerFor, setPickerFor] = React.useState<number | null>(null);
  const [busy, setBusy] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    setPlan(null);
    getConsentPlan(workflowId, version)
      .then((p) => setPlan({ declaredScopes: p.declaredScopes, grants: p.grants }))
      .catch((e) =>
        toast({
          variant: "destructive",
          title: "同意内容の取得に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        }),
      );
  }, [open, workflowId, version]);

  const unresolved = (plan?.grants ?? []).filter(
    (g) => g.needsUserPick && !g.picked && g.objectKind === "folder",
  );

  const enable = async () => {
    if (!plan) return;
    setBusy(true);
    try {
      const grants: GrantInput[] = plan.grants
        .map((g) => {
          const objectId = g.picked?.id ?? g.objectId;
          if (!objectId) return null;
          return {
            scope: g.scope,
            object_type: g.objectKind,
            object_id: objectId,
            relation: g.relation,
          };
        })
        .filter((g): g is GrantInput => g !== null);
      await enableWorkflow(workflowId, version, grants);
      toast({ title: "自動実行を有効にしました" });
      onChanged();
      onOpenChange(false);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "有効化できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(false);
    }
  };

  const disable = async () => {
    setBusy(true);
    try {
      await disableWorkflow(workflowId);
      toast({ title: "自動実行を無効にしました" });
      onChanged();
      onOpenChange(false);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "無効化できませんでした",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setBusy(false);
    }
  };

  const reconsent = registration?.status === "suspended_reconsent";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldCheck className="size-5 text-primary" aria-hidden />
            自動実行の設定
          </DialogTitle>
          <DialogDescription>
            スケジュールやできごとで動かすには、あなたの権限の範囲でこのワークフローに
            権限を渡します（v{version} を有効化）。
          </DialogDescription>
        </DialogHeader>

        {reconsent ? (
          <p className="flex items-start gap-2 rounded-md border border-[oklch(0.7_0.12_80)]/50 bg-[oklch(0.94_0.06_80)]/50 p-2.5 text-xs text-[oklch(0.4_0.1_70)] dark:bg-[oklch(0.32_0.06_80)]/50 dark:text-[oklch(0.87_0.09_85)]">
            <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
            権限の変更により自動実行が停止しています。内容を確認して、もう一度有効化してください。
          </p>
        ) : null}

        {plan === null ? (
          <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            確認しています…
          </div>
        ) : (
          <div className="space-y-4">
            <div>
              <h3 className="mb-1.5 text-xs font-semibold text-muted-foreground">
                このワークフローができること
              </h3>
              {plan.declaredScopes.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  データへのアクセス権限は使いません
                </p>
              ) : (
                <ul className="flex flex-wrap gap-1.5">
                  {plan.declaredScopes.map((s) => (
                    <li key={s}>
                      <Badge variant="secondary">{SCOPE_LABELS[s] ?? s}</Badge>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            {plan.grants.length > 0 ? (
              <div>
                <h3 className="mb-1.5 text-xs font-semibold text-muted-foreground">
                  渡す権限（あなたが持っている範囲だけ渡せます）
                </h3>
                <ul className="space-y-1.5">
                  {plan.grants.map((g, i) => (
                    <li
                      key={`${g.scope}:${g.objectId ?? g.objectName ?? i}`}
                      className="flex items-center gap-2 rounded-md border px-2.5 py-2 text-xs"
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium">
                          {SCOPE_LABELS[g.scope] ?? g.scope}
                          <span className="ml-1 text-muted-foreground">
                            （{RELATION_LABELS[g.relation] ?? g.relation}）
                          </span>
                        </span>
                        <span className="block truncate text-muted-foreground">
                          {g.picked
                            ? `フォルダ「${g.picked.name}」`
                            : g.objectName
                              ? `「${g.objectName}」`
                              : g.objectId
                                ? `ID: ${g.objectId}`
                                : "対象を選んでください"}
                        </span>
                      </span>
                      {g.needsUserPick && g.objectKind === "folder" ? (
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-7 shrink-0 text-xs"
                          onClick={() => setPickerFor(i)}
                        >
                          <FolderOpen className="size-3.5" aria-hidden />
                          {g.picked ? "変更" : "選ぶ"}
                        </Button>
                      ) : null}
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}

            <div className="flex items-center justify-between gap-2 border-t pt-3">
              {registration?.status === "enabled" ? (
                <Button variant="outline" size="sm" onClick={disable} disabled={busy}>
                  自動実行を止める
                </Button>
              ) : (
                <span />
              )}
              <Button onClick={enable} disabled={busy || unresolved.length > 0}>
                {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                {unresolved.length > 0
                  ? `対象を選択してください（残り ${unresolved.length}）`
                  : reconsent
                    ? "もう一度有効化する"
                    : "同意して有効化"}
              </Button>
            </div>
          </div>
        )}

        <FolderPicker
          open={pickerFor !== null}
          onOpenChange={(o) => !o && setPickerFor(null)}
          onSelect={(choice) => {
            if (pickerFor !== null && plan) {
              const grants = [...plan.grants];
              grants[pickerFor] = {
                ...grants[pickerFor],
                picked: { id: choice.id, name: choice.name },
              };
              setPlan({ ...plan, grants });
            }
            setPickerFor(null);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
