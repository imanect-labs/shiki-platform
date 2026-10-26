"use client";

/// 有効化（同意）ダイアログ（Task 10.4a/10.12・engine.md §10）。
///
/// スケジュール/イベントで自動実行するには、**自分の権限の範囲から**ワークフローに
/// 権限を渡す（委譲）必要がある。consent-plan（IR 静的分析）の提案を日本語で見せ、
/// 対象未確定のものはテーブル・フォルダ・ファイルのピッカーで選ばせてから enable する。

import * as React from "react";
import { AlertTriangle, FileText, FolderOpen, Loader2, ShieldCheck } from "lucide-react";

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
import { cn } from "@/lib/utils";
import { SCOPE_LABELS } from "@/lib/workflow-scopes";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import { DrivePicker } from "@/components/chat/drive-picker";
import { DataTableSelect, UNREADABLE_TABLE, useDataTables } from "@/components/workflow/editor/config-panel/data-tables";
import { DriveItemName, isCsvFile } from "@/components/workflow/editor/config-panel/forms-files";
import {
  disableWorkflow,
  enableWorkflow,
  getConsentPlan,
  getWorkflowVersion,
  type GrantInput,
  type Registration,
  type SuggestedGrant,
  type ValidationError,
} from "@/lib/workflow-api";


const RELATION_LABELS: Record<string, string> = {
  viewer: "閲覧",
  editor: "編集",
  can_use: "使用",
};

/// 委譲の対象の種類ごとの呼び名と、対象を決める params の項目（`dynamic` の判定に使う）。
/// フォルダは判定しない: 親フォルダに渡した権限は配下にも効くので、前の結果で決まるフォルダでも
/// 親を選んで渡す意味がある（テーブルとファイルには階層が無い）。
const TARGETS: Record<string, { what: string; param?: string }> = {
  data_table: { what: "テーブル", param: "table" },
  file: { what: "ファイル", param: "file" },
  folder: { what: "フォルダ" },
};

type GrantRow = SuggestedGrant & {
  picked?: { id: string; name: string };
  /// 対象（テーブル・ファイル）を実行の入力など前の結果から決めるブロック（`$from`）の権限。
  /// 有効化の時点では対象が決まらないので選ばせない（選ぶと権限だけ渡り、実行の対象とは無関係になる・C-P2-2）。
  /// `from` は決め方（実行の入力・きっかけの内容・前のブロックの結果）。
  dynamic?: { label: string; from: "input" | "trigger" | "node"; what: string };
};

/// 対象を前の結果から決めるブロックの説明（BUG-38）。きっかけ（ファイル保存）で決まるファイルは、きっかけの
/// フォルダに渡す閲覧の権限で読める（ファイルの権限はフォルダから受け継ぐ）。
function dynamicText(g: GrantRow & { dynamic: NonNullable<GrantRow["dynamic"]> }, schedule: boolean): string {
  const { label, from, what } = g.dynamic;
  // テーブルを対象にするのはデータ変更のきっかけ（変更された行のテーブル）だけ。
  if (from === "trigger" && g.objectKind === "data_table") {
    return g.relation === "viewer"
      ? `「${label}」の${what}はきっかけ（変更された行のテーブル）で決まるため、ここでは選びません。きっかけのテーブルに渡す閲覧の権限で読めます。`
      : `「${label}」の${what}はきっかけ（変更された行のテーブル）で決まるため、ここでは選びません。書き換えるには、そのテーブルの編集の権限が要ります。`;
  }
  if (from === "trigger") {
    return g.relation === "viewer"
      ? `「${label}」の${what}はきっかけ（保存されたファイル）で決まるため、ここでは選びません。きっかけのフォルダに渡す閲覧の権限で読めます。`
      : `「${label}」の${what}はきっかけ（保存されたファイル）で決まるため、ここでは選びません。書き換えるには、そのフォルダの編集の権限が要ります。`;
  }
  const source = from === "input" ? "実行の入力" : "前のブロックの結果";
  const advice =
    schedule && from === "input"
      ? `スケジュールの実行には入力が無いので、このブロックは失敗します。自動で動かすならブロックで${what}を${g.objectKind === "data_table" ? "一覧" : "ドライブ"}から選んでください。`
      : `自動で動かすときは、その${what}を使える権限が要ります。`;
  return `「${label}」の${what}は${source}で決まるため、ここでは渡せません。${advice}`;
}

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
    /// 有効化できない理由（必須の文字の欄が空・無人の実行の保存先の省略・D-40・D-43）。
    blockers: ValidationError[];
    /// ブロックの表示名（理由の見出し）。
    labels: Map<string, string>;
  } | null>(null);
  const [pickerFor, setPickerFor] = React.useState<number | null>(null);
  const [filePickerFor, setFilePickerFor] = React.useState<number | null>(null);
  const [busy, setBusy] = React.useState(false);
  /// スケジュールで動くか（入力の無い実行で「入力からテーブルを決める」ブロックは動かない）。
  const [schedule, setSchedule] = React.useState(false);
  /// データ変更で起動するか・承認を待つか（有効化の前に、値の残り方と受信箱を説明する・GAP-07・GAP-08）。
  const [dataTrigger, setDataTrigger] = React.useState(false);
  const [approval, setApproval] = React.useState(false);
  // 業務データのテーブル（data_table の委譲先を選ぶ・名前の表示にも使う）。
  const tableGrants = (plan?.grants ?? []).filter((g) => g.objectKind === "data_table");
  // ブロックが束縛したテーブルが一覧（200 件で打ち切る）に無くても、1 件取って名前を出す（BUG-68）。
  const tableSource = useDataTables({
    enabled: open && tableGrants.length > 0,
    bound: tableGrants.map((g) => g.objectId),
  });
  const tableName = (id: string | null) => {
    const found = tableSource.lookup(id);
    return found.state === "ok" ? found.table.name : undefined;
  };
  const tableLabel = (id: string) => {
    const found = tableSource.lookup(id);
    return found.state === "unreadable" ? `${UNREADABLE_TABLE}: ${id}` : `テーブル「${tableName(id) ?? id}」`;
  };
  const pick = (i: number, picked: { id: string; name: string }) => {
    if (!plan) return;
    const grants = [...plan.grants];
    grants[i] = { ...grants[i], picked };
    setPlan({ ...plan, grants });
  };

  React.useEffect(() => {
    if (!open) return;
    setPlan(null);
    Promise.all([getConsentPlan(workflowId, version), getWorkflowVersion(workflowId, version).catch(() => null)])
      .then(([p, wf]) => {
        // 提案の出どころ（`node:<id>`）のブロックが、対象を前の結果から決めているか。
        const nodes = new Map((wf?.ir.nodes ?? []).map((n) => [n.id, n]));
        setSchedule(Boolean(wf?.ir.triggers.some((t) => t.kind === "schedule")));
        setDataTrigger(
          Boolean(wf?.ir.triggers.some((t) => t.kind === "event" && String(t.source).startsWith("data."))),
        );
        setApproval(Boolean(wf?.ir.nodes.some((n) => n.type === "human.approval")));
        const grants: GrantRow[] = p.grants.map((g) => {
          const node = g.source.startsWith("node:") ? nodes.get(g.source.slice(5)) : undefined;
          const target = TARGETS[g.objectKind];
          const value = target?.param ? (node?.params as Record<string, unknown> | undefined)?.[target.param] : undefined;
          if (target && g.needsUserPick && value && typeof value === "object" && "$from" in value) {
            const from = String((value as { $from: unknown }).$from);
            return {
              ...g,
              dynamic: {
                label: node?.label || node?.id || "",
                from: from === "input" ? "input" : from === "trigger" ? "trigger" : "node",
                what: target.what,
              },
            } satisfies GrantRow;
          }
          return g;
        });
        const labels = new Map((wf?.ir.nodes ?? []).map((n) => [n.id, n.label || n.id]));
        setPlan({ declaredScopes: p.declaredScopes, grants, blockers: p.blockers, labels });
      })
      .catch((e) =>
        toast({
          variant: "destructive",
          title: "同意内容の取得に失敗しました",
          description: e instanceof Error ? e.message : String(e),
        }),
      );
  }, [open, workflowId, version]);

  // 対象未確定の grant は種類を問わず有効化をブロックする（fail-closed）。
  // ピッカーを出せるのはテーブル・フォルダ・ファイル。それ以外（将来の種類）は保存側の見直しを促す。
  const unresolved = (plan?.grants ?? []).filter((g) => g.needsUserPick && !g.picked && !g.dynamic);
  const blockers = plan?.blockers ?? [];

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
            {`スケジュールやできごとで動かすには、あなたの権限の範囲でこのワークフローに権限を渡します（v${version} を有効化）。`}
          </DialogDescription>
        </DialogHeader>

        {reconsent ? (
          <p className="flex items-start gap-2 rounded-md border p-2.5 text-xs text-foreground/80" style={{ borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)", backgroundColor: "color-mix(in oklab, var(--season-autumn) 10%, transparent)" }}>
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

            {dataTrigger ? (
              <p className="text-xs leading-relaxed text-muted-foreground" data-testid="enable-data-trigger-note">
                テーブルの行が変わったときに動きます。あなたに見える行・列の変更だけで動き、変更された行の、あなたに
                見える値が実行履歴に残ります（実行履歴はこのワークフローを見られる人が読めます）。
              </p>
            ) : null}
            {approval ? (
              <p className="text-xs leading-relaxed text-muted-foreground" data-testid="enable-approval-note">
                承認を待つ間、担当者の受信箱（サイドバーの「承認」）に出ます。担当者が判断するには、そのテーブルの編集
                権限と、状態の遷移の規則で許されていることが要ります。
              </p>
            ) : null}

            {blockers.length > 0 ? (
              <div
                role="alert"
                className="space-y-1.5 rounded-md border border-destructive/40 bg-destructive/5 p-2.5 text-xs"
                data-testid="enable-blockers"
              >
                <p className="flex items-center gap-1.5 font-medium text-destructive">
                  <AlertTriangle className="size-4 shrink-0" aria-hidden />
                  このままでは有効化できません。エディタで次の設定を直してから、もう一度開いてください。
                </p>
                <ul className="list-disc space-y-1 pl-5 text-foreground/80">
                  {blockers.map((b, i) => (
                    <li key={i}>
                      {b.node_id ? <span className="font-medium">「{plan.labels.get(b.node_id) ?? b.node_id}」</span> : null}
                      {b.message}
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}

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
                        <span className={cn("block text-muted-foreground", !g.dynamic && "truncate")}>
                          {g.dynamic
                            ? dynamicText({ ...g, dynamic: g.dynamic }, schedule)
                            : g.picked
                            ? `${TARGETS[g.objectKind]?.what ?? ""}「${g.picked.name}」`
                            : g.objectKind === "data_table" && g.objectId
                            ? tableLabel(g.objectId)
                            : (g.objectKind === "file" || g.objectKind === "folder") && g.objectId
                            ? (
                                <>
                                  {TARGETS[g.objectKind].what}「
                                  <DriveItemName id={g.objectId} fallback={g.objectId} />」
                                </>
                              )
                            : g.objectName
                              ? `「${g.objectName}」`
                              : g.objectId
                                ? `ID: ${g.objectId}`
                                : "対象を選んでください"}
                        </span>
                      </span>
                      {g.dynamic ? (
                        <AlertTriangle className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                      ) : null}
                      {g.needsUserPick && g.objectKind === "data_table" && !g.dynamic ? (
                        <div className="w-40 shrink-0">
                          <DataTableSelect
                            label="渡すテーブル"
                            value={g.picked?.id ?? null}
                            source={tableSource}
                            onChange={(id) => pick(i, { id, name: tableName(id) ?? id })}
                          />
                        </div>
                      ) : null}
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
                      {g.needsUserPick && g.objectKind === "file" && !g.dynamic ? (
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-7 shrink-0 text-xs"
                          aria-label={`渡すファイルを${g.picked ? "変更" : "選ぶ"}`}
                          onClick={() => setFilePickerFor(i)}
                        >
                          <FileText className="size-3.5" aria-hidden />
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
              <Button onClick={enable} disabled={busy || unresolved.length > 0 || blockers.length > 0}>
                {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                {blockers.length > 0
                  ? `設定を直してください（${blockers.length} 件）`
                  : unresolved.length > 0
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
            if (pickerFor !== null) pick(pickerFor, { id: choice.id, name: choice.name });
            setPickerFor(null);
          }}
        />
        <DrivePicker
          open={filePickerFor !== null}
          onOpenChange={(o) => !o && setFilePickerFor(null)}
          title="渡すファイルを選ぶ"
          description="このワークフローに権限を渡すファイルを選んでください。"
          accept={
            filePickerFor !== null && plan?.grants[filePickerFor]?.scope.startsWith("csv.") ? isCsvFile : undefined
          }
          onSelect={(n) => {
            if (filePickerFor !== null) pick(filePickerFor, { id: n.id, name: n.name });
            setFilePickerFor(null);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
