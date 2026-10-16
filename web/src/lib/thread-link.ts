/// 会話（スレッド）を開く先の解決（サイドバー履歴・検索パレットで共有・#598）。
///
/// 文書の横で始めた会話は `origin_note_id` に文書の node id を持つ。ノートも Office 文書も
/// 同じ欄を使うため、作成時に控えた文書名（`originNoteName`）の拡張子で開く画面を分ける。
/// 拡張子→ルートの対応は `resource-link.ts`（ドライブの open ディスパッチ）と揃える。

import type { Thread } from "@/lib/chat-api";

/// 会話の由来。`null` は通常のチャット（または開く画面を持たない文書）。
export type ThreadOriginKind = "note" | "office" | null;

const OFFICE_EXTENSIONS = [".docx", ".xlsx", ".pptx", ".odt", ".ods", ".odp"];

export function threadOriginKind(
  thread: Pick<Thread, "originNoteId" | "originNoteName">,
): ThreadOriginKind {
  if (!thread.originNoteId) return null;
  const name = (thread.originNoteName ?? "").toLowerCase();
  if (OFFICE_EXTENSIONS.some((ext) => name.endsWith(ext))) return "office";
  // サーバは id と名前を必ず対で入れるが、名前が欠けていたら従来どおりノートとして扱う。
  if (name === "" || name.endsWith(".md")) return "note";
  return null;
}

/// 由来文書の画面のパス（その会話をアクティブにする `?thread=` 付き）。通常のチャットは `/c/{id}`。
export function threadHref(
  thread: Pick<Thread, "id" | "originNoteId" | "originNoteName">,
): string {
  switch (threadOriginKind(thread)) {
    case "note":
      return `/notes/${thread.originNoteId}?thread=${thread.id}`;
    case "office":
      return `/office/${thread.originNoteId}?thread=${thread.id}`;
    default:
      return `/c/${thread.id}`;
  }
}
