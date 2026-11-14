"use client";

/// ワークフローエディタの「AI に依頼」パネル（D-10 T10・GAP-33）。
///
/// 文書のアシスタントと同じ [`DocChatPanel`] を、会話の対象＝このワークフロー（`origin_artifact`）で使う。
/// アクティブな会話は Office と同じく localStorage に持つ（ワークフローは Yjs を持たない）。
/// サーバは会話の run の開始時に、このワークフローの今の版を発話者の権限で読み直して AI に伝え、
/// 保存（emit_workflow）は読んだ版を expected_version に渡させる。保存の知らせ（`onWorkflowSaved`）で
/// ページがエディタを読み直すか、未保存の変更があれば帯で選ばせる。

import { Loader2, Save } from "lucide-react";
import * as React from "react";

import { PanelEmptyHint } from "@/components/chat/doc-chat-draft";
import { DocChatPanel, useLocalActiveThreadStore } from "@/components/chat/doc-chat-panel";
import { Button } from "@/components/ui/button";
import { useMe } from "@/hooks/use-me";
import { useStorageScopeReady } from "@/lib/user-storage";

/// エディタの各所（上部バー・設定欄の「AI 編集で…」）からパネルを開く口。
export type WorkflowAssistant = {
  open: boolean;
  setOpen: (open: boolean) => void;
};

const WorkflowAssistantContext = React.createContext<WorkflowAssistant | null>(null);

export const WorkflowAssistantProvider = WorkflowAssistantContext.Provider;

/// パネルを開く口（AI のパネルを持たない画面では null）。
export function useWorkflowAssistant(): WorkflowAssistant | null {
  return React.useContext(WorkflowAssistantContext);
}

/// 設定欄の「AI 編集で変更できます」の横に置く「AI に依頼」（文言と実態をそろえる）。パネルが無い画面では出さない。
export function AskAiButton({ className }: { className?: string }) {
  const assistant = useWorkflowAssistant();
  if (!assistant) return null;
  return (
    <button
      type="button"
      onClick={() => assistant.setOpen(true)}
      className={
        className ??
        "ml-1 inline rounded-sm font-medium text-foreground underline underline-offset-2 outline-none hover:text-primary focus-visible:ring-2 focus-visible:ring-ring"
      }
      data-testid="workflow-ask-ai-inline"
    >
      AI に依頼
    </button>
  );
}

export function WorkflowChatPanel({
  workflowId,
  name,
  initialThreadId,
  dirty,
  onSave,
  onWorkflowSaved,
}: {
  workflowId: string;
  /// 表示名（会話の題名の接頭に使う）。
  name: string;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
  /// エディタに保存していない変更があるか。
  dirty: boolean;
  /// エディタの変更を保存する（保存できたら true）。
  onSave: () => Promise<boolean>;
  onWorkflowSaved: (ref: { id: string; version: number }) => void;
}) {
  const store = useLocalActiveThreadStore(`workflow:${workflowId}`);
  // アクティブ会話 id はユーザー単位の localStorage にある（#817）。確定する前に読むと続きの会話ではなく
  // 新しい会話の入力欄を出してしまうので、確定まで待つ（Office のパネルと同じ）。
  const scopeReady = useStorageScopeReady();
  const me = useMe();
  const [saving, setSaving] = React.useState(false);
  const save = React.useCallback(async () => {
    setSaving(true);
    try {
      await onSave();
    } finally {
      setSaving(false);
    }
  }, [onSave]);

  if (!scopeReady && me.error) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-4 text-center text-sm" role="alert">
        <p className="text-destructive">チャットを準備できませんでした。</p>
        <button
          type="button"
          onClick={me.reload}
          className="mt-2 text-muted-foreground underline underline-offset-2 hover:text-foreground"
        >
          再試行
        </button>
      </div>
    );
  }
  if (!scopeReady) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        チャットを準備しています…
      </div>
    );
  }
  return (
    <DocChatPanel
      store={store}
      nodeId={workflowId}
      originKind="workflow"
      title={`ワークフロー: ${name}`}
      label="このワークフローの会話"
      // 開けている（＝viewer 以上）なら会話を作れる。保存は emit_workflow がサーバで editor を確かめる。
      editable
      testIdPrefix="workflow"
      initialThreadId={initialThreadId}
      onWorkflowSaved={onWorkflowSaved}
      hint={
        <PanelEmptyHint
          title="このワークフローの直し方を頼めます"
          description="例:「承認されたら担当者にメールで知らせて」「金額が 10 万円を超えたら部長の承認を足して」。AI が保存すると、この画面に新しい版が読み込まれます。"
        />
      }
      notice={
        dirty ? (
          <div
            role="status"
            className="mx-2 mb-1 mt-1.5 flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-[color-mix(in_oklab,var(--season-autumn)_45%,transparent)] bg-[color-mix(in_oklab,var(--season-autumn)_8%,transparent)] px-3 py-2 text-[12px] leading-relaxed"
            data-testid="workflow-assistant-unsaved"
          >
            <span className="min-w-0 flex-1">
              保存していない変更があります。AI は保存済みの最新版を読んで直すので、この変更は入りません。
            </span>
            <Button size="sm" variant="outline" className="h-7 px-2 text-[12px]" onClick={() => void save()} disabled={saving}>
              {saving ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : <Save className="size-3.5" aria-hidden />}
              先に保存する
            </Button>
          </div>
        ) : null
      }
    />
  );
}
