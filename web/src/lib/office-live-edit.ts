"use client";

/// 開いている Office セッションへの AI ライブ編集の受け渡し emitter（#328）。
///
/// チャットの [`Conversation`] が SSE `office_live_edit` を受けると publish し、`/office/[id]`
/// ページが自分の fileId 分だけ購読して [`OfficeEditor.applyLiveEdit`]（Collabora の
/// `Action_Paste` で現在の選択を置換）を実行する。ページ内のチャット⇄エディタを疎結合にする
/// 一方向イベント（[`selection-context`] と同じ流儀・永続化しない）。ライブ専用のため履歴には
/// 残らず（サーバが content へ projection しない）、再生で二重 paste しない。

export interface OfficeLiveEdit {
  /// 対象 Office ファイルの storage node id。
  node_id: string;
  /// 現在の選択範囲を置き換えるサニタイズ済み HTML。
  html: string;
}

const listeners = new Set<(edit: OfficeLiveEdit) => void>();

/// ライブ編集を配信する（購読者＝開いている /office ページが自分の fileId 分を拾う）。
export function publishOfficeLiveEdit(edit: OfficeLiveEdit): void {
  for (const l of listeners) l(edit);
}

/// ライブ編集を購読する（返り値で解除）。
export function subscribeOfficeLiveEdit(cb: (edit: OfficeLiveEdit) => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}
