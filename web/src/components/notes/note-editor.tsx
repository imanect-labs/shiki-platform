"use client";

/// ノート本文エディタ（Task 11P.3・TipTap + y-prosemirror）。
///
/// - 真実は Yjs（Collaboration が field "default" に束縛・undo も Yjs 側）。
/// - リモートカーソル/プレゼンスは CollaborationCaret（awareness）。
/// - viewer は editable=false（強制はサーバ側: update 不受理・定期再チェック）。

import Collaboration from "@tiptap/extension-collaboration";
import CollaborationCaret from "@tiptap/extension-collaboration-caret";
import Link from "@tiptap/extension-link";
import Placeholder from "@tiptap/extension-placeholder";
import { Table } from "@tiptap/extension-table";
import { TableCell } from "@tiptap/extension-table-cell";
import { TableHeader } from "@tiptap/extension-table-header";
import { TableRow } from "@tiptap/extension-table-row";
import { TaskItem } from "@tiptap/extension-task-item";
import { TaskList } from "@tiptap/extension-task-list";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import * as React from "react";

import type { CollabProvider } from "@/lib/collab";
import { createSlashCommand, type SlashItem } from "./slash-command";

/// 参加者カーソルの配色（design token に寄せた識別しやすい 8 色）。
export const PRESENCE_COLORS = [
  "#2563eb",
  "#db2777",
  "#16a34a",
  "#ea580c",
  "#7c3aed",
  "#0891b2",
  "#ca8a04",
  "#dc2626",
];

/// ユーザー id から安定した色を選ぶ（同一ユーザーは常に同色）。
export function presenceColor(userId: string): string {
  let hash = 0;
  for (const ch of userId) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return PRESENCE_COLORS[hash % PRESENCE_COLORS.length];
}

export interface NoteEditorProps {
  provider: CollabProvider;
  editable: boolean;
  user: { id: string; name: string };
  /// スラッシュメニューへ追加する項目（11P.5 の AI アクション・11P.6 の埋め込み）。
  extraSlashItems?: () => SlashItem[];
}

export function NoteEditor({ provider, editable, user, extraSlashItems }: NoteEditorProps) {
  const extensions = React.useMemo(
    () => [
      StarterKit.configure({
        // 履歴は Yjs（Collaboration）が持つ（ローカル履歴と二重管理しない）。
        undoRedo: false,
        link: false,
      }),
      Link.configure({
        openOnClick: false,
        autolink: true,
        // javascript: 等のスキームを拒否（安全側の既定を明示）。
        protocols: ["http", "https", "mailto"],
      }),
      Table.configure({ resizable: false }),
      TableRow,
      TableHeader,
      TableCell,
      TaskList,
      TaskItem.configure({ nested: true }),
      Placeholder.configure({
        placeholder: "入力するか、「/」でコマンドを呼び出す…",
      }),
      Collaboration.configure({ document: provider.doc, field: "default" }),
      CollaborationCaret.configure({
        provider,
        user: { name: user.name, color: presenceColor(user.id) },
      }),
      createSlashCommand(extraSlashItems ?? (() => [])),
    ],
    // provider/user は同一ノートのライフサイクルで不変（ページが key で再マウントする）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [provider],
  );

  const editor = useEditor(
    {
      extensions,
      editable,
      // SSR ハイドレーション不整合を避ける（Yjs 内容はクライアントでのみ確定する）。
      immediatelyRender: false,
      editorProps: {
        attributes: {
          class: "note-prose focus:outline-none",
          "data-testid": "note-editor",
        },
      },
    },
    [extensions],
  );

  React.useEffect(() => {
    editor?.setEditable(editable);
  }, [editor, editable]);

  return (
    <div className="note-editor-root min-h-[50vh]">
      <EditorContent editor={editor} />
    </div>
  );
}
