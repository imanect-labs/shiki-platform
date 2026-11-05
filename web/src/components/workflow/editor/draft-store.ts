/// エディタの未保存の編集を、アプリ内の画面移動をまたいで残す（実行履歴を見て戻ると消えていた）。
///
/// - モジュール内のメモリにだけ置く（同じタブの SPA 遷移の間だけ生きる）。再読み込み・タブを閉じる
///   操作は従来どおり `beforeunload` の確認で止める。localStorage に残さないのは、端末に IR
///   （プロンプト・URL 等）を残さないためと、保存済みの版より古い下書きを後から復元しないため。
/// - 下書きは編集を始めた保存済みの版（`baseVersion`）と組で持つ。戻ったときサーバの版が進んでいたら
///   （他の人・別タブが保存した）復元しない（そのまま保存しても 409 になるだけのため）。
import type { WorkflowIr } from "@/generated/workflow-ir";

export type WorkflowDraft = {
  baseVersion: number;
  ir: WorkflowIr;
  undoStack: WorkflowIr[];
  redoStack: WorkflowIr[];
};

const drafts = new Map<string, WorkflowDraft>();

export const workflowDraftStore = {
  get(workflowId: string): WorkflowDraft | undefined {
    return drafts.get(workflowId);
  },
  set(workflowId: string, draft: WorkflowDraft): void {
    drafts.set(workflowId, draft);
  },
  clear(workflowId: string): void {
    drafts.delete(workflowId);
  },
};

/// 開いた版に対して使える下書きを取り出す。版が合わない下書きは捨てて、捨てたことを返す。
export function takeDraft(
  workflowId: string,
  version: number,
): { draft: WorkflowDraft | null; discarded: boolean } {
  const draft = drafts.get(workflowId);
  if (!draft) return { draft: null, discarded: false };
  if (draft.baseVersion !== version) {
    drafts.delete(workflowId);
    return { draft: null, discarded: true };
  }
  return { draft, discarded: false };
}
