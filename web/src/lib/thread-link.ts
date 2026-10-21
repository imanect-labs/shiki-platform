/// 会話（スレッド）を開く先の解決（サイドバー履歴・検索パレットで共有・#598）。
///
/// 文書の横で始めた会話は `origin_note_id` に文書の node id を持つ。ノートも Office 文書も
/// 同じ欄を使うため、作成時に控えた文書名（`originNoteName`）の拡張子で開く画面を分ける。
/// 拡張子→ルートの対応は `resource-link.ts`（ドライブの open ディスパッチ）と揃える。
///
/// 文書名は作成時のスナップショットで、**拡張子を変える改名には追随しない（意図的）**。会話の中で
/// AI が使う文書ツールも同じ名前で選ぶ（`crates/chat/src/worker/history.rs`）ので、画面とツールを
/// 揃える。現在の名前で判定しても、中身は変わらないため正しく開けるようにはならない（#616）。

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

/// 文書の横で始めた会話のタイトルに付く接頭（`note-chat-panel.tsx`「ノート: 」・
/// `office-chat-panel.tsx`「文書: 」が付与）。履歴では種類アイコンと 2 行目で由来を示すので外す。
const DOC_TITLE_PREFIX = /^(?:ノート|文書)[:：]\s*/u;
const DOC_EXTENSION = /\.(?:md|docx|xlsx|pptx|odt|ods|odp)$/iu;

/// 履歴・検索パレットの 1 行に出す表示用の情報。
export type ThreadDisplay = {
  kind: ThreadOriginKind;
  /// 表示用タイトル（文書由来は接頭「ノート: 」等を外したもの）。
  title: string;
  /// 文書由来の補足行（「ノートの会話」等）。通常のチャットは null。
  detail: string | null;
};

export function threadDisplay(
  thread: Pick<Thread, "title" | "originNoteId" | "originNoteName">,
): ThreadDisplay {
  const kind = threadOriginKind(thread);
  if (kind === null) return { kind, title: thread.title, detail: null };
  const title = thread.title.replace(DOC_TITLE_PREFIX, "") || thread.title;
  const docName = (thread.originNoteName ?? "").replace(DOC_EXTENSION, "");
  const noun = kind === "note" ? "ノート" : "文書";
  // 下書きを保存して文書由来になった会話はタイトルが元のチャットのまま。そのときは文書名を補う。
  const detail =
    docName && !title.startsWith(docName) ? `${noun}「${docName}」の会話` : `${noun}の会話`;
  return { kind, title, detail };
}
