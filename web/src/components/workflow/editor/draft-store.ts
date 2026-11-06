/// エディタの未保存の編集を、アプリ内の画面移動をまたいで残す（実行履歴を見て戻ると消えていた）。
///
/// - モジュール内のメモリにだけ置く（同じタブの SPA 遷移の間だけ生きる）。再読み込み・タブを閉じる
///   操作は従来どおり `beforeunload` の確認で止める。localStorage に残さないのは、端末に IR
///   （プロンプト・URL 等）を残さないためと、保存済みの版より古い下書きを後から復元しないため。
/// - 下書きは編集を始めた保存済みの版（`baseVersion`）と組で持つ。戻ったときサーバの版が進んでいたら
///   （他の人・別タブが保存した）復元しない（そのまま保存しても 409 になるだけのため）。
/// - ログイン中のユーザーが変わったら（他タブのログアウトを含む・`user-storage` のスコープ）全部捨てる。
///   自タブのログアウトはページ全体の遷移なのでメモリごと消える。
/// - 件数に上限を置く（タブを長く開いたまま多くのワークフローを渡り歩いても増え続けない）。
import type { WorkflowIr } from "@/generated/workflow-ir";
import type { EditorLayout } from "@/lib/workflow-api";
import { scopedStorageKey, subscribeStorageScope } from "@/lib/user-storage";

export type WorkflowDraft = {
  baseVersion: number;
  ir: WorkflowIr;
  /// IR に未保存の編集があるか（座標だけの下書きでは false・IR は復元しない）。
  irDirty: boolean;
  /// 移動の直前に動かした座標（自動保存の debounce 中に離れても失わない）。
  layout: EditorLayout;
  /// 座標が未保存か（false なら復元せず、保存済みの配置を使う）。
  layoutDirty: boolean;
  undoStack: WorkflowIr[];
  redoStack: WorkflowIr[];
};

/// 残す下書きの上限（古く触ったものから捨てる）。
const MAX_DRAFTS = 10;

const drafts = new Map<string, WorkflowDraft>();
/// 進行中の保存（エディタを離れた後も続く）。開き直す前にこれを待ち、保存の結果を下書きに反映してから
/// サーバの版を読む（待たないと、自分の保存で進んだ版を「他の人の保存」と取り違えて下書きを捨てたり、
/// 保存前の版で開いて次の保存が競合したりする）。
const pendingSaves = new Map<string, Promise<unknown>>();

// スコープ（ログイン中のユーザー）が変わったら捨てる。未確定（null）との行き来も変化として扱う
// （未確定のうちに作った下書きの持ち主は分からないため。/me は開いてすぐ確定するので実害は無い）。
let draftScope: string | null = scopedStorageKey("");
subscribeStorageScope(() => {
  const next = scopedStorageKey("");
  if (next !== draftScope) drafts.clear();
  draftScope = next;
});

export const workflowDraftStore = {
  set(workflowId: string, draft: WorkflowDraft): void {
    // 触った順に並べ直す（Map は挿入順）。
    drafts.delete(workflowId);
    drafts.set(workflowId, draft);
    while (drafts.size > MAX_DRAFTS) {
      const oldest = drafts.keys().next().value;
      if (oldest === undefined) break;
      drafts.delete(oldest);
    }
  },
  /// 保存の要求を登録する（終われば外す）。
  trackSave(workflowId: string, request: Promise<unknown>): void {
    const settled = request.then(
      () => undefined,
      () => undefined,
    );
    pendingSaves.set(workflowId, settled);
    void settled.then(() => {
      if (pendingSaves.get(workflowId) === settled) pendingSaves.delete(workflowId);
    });
  },
  /// 進行中の保存が終わる（成否は問わない）のを待つ。無ければすぐ終わる。
  settled(workflowId: string): Promise<unknown> {
    return pendingSaves.get(workflowId) ?? Promise.resolve();
  },
  clear(workflowId: string): void {
    drafts.delete(workflowId);
  },
  /// 保存が終わった（画面を離れた後に終わった場合も含む）。送った内容のままなら下書きを消し、
  /// 保存中に続けた編集があれば、新しい版から始めた下書きとして付け替える。
  saved(workflowId: string, fromVersion: number, savedIr: WorkflowIr, version: number): void {
    const draft = drafts.get(workflowId);
    if (!draft || draft.baseVersion !== fromVersion) return;
    // 送った内容のままなら IR は保存済み。座標が未保存なら、座標だけの下書きとして残す。
    const irDirty = draft.ir !== savedIr;
    if (!irDirty && !draft.layoutDirty) drafts.delete(workflowId);
    else drafts.set(workflowId, { ...draft, baseVersion: version, irDirty });
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
