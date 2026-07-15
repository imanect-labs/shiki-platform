"use client";

/// 下書きノートのエディタ（issue #282・クライアント内のみ・サーバ同期なし）。
///
/// 通常の [`NoteEditor`] は Yjs（Collaboration）が真実だが、下書きは**保存前のクライアント
/// 状態**なので Collaboration を外し、ローカル履歴（StarterKit undoRedo）で 1 人編集する。
/// 本文は Markdown を真実源（下書きストア）とし、seed で流し込み（AI 生成/タブ切替）、編集は
/// `onChangeMarkdown` で書き戻す。保存時は `onReady` で受けたエディタから md を直列化する。
///
/// 拡張は NoteEditor と揃える（ツールバー/スラッシュ/埋め込み/ライブプレビュー）。Collaboration
/// 系のみ差し替え。`seed.nonce` が変わったときだけ本文を再シードする（手編集では再シードしない）。

import Link from "@tiptap/extension-link";
import Placeholder from "@tiptap/extension-placeholder";
import { Table } from "@tiptap/extension-table";
import { TableCell } from "@tiptap/extension-table-cell";
import { TableHeader } from "@tiptap/extension-table-header";
import { TableRow } from "@tiptap/extension-table-row";
import { TaskItem } from "@tiptap/extension-task-item";
import { TaskList } from "@tiptap/extension-task-list";
import { EditorContent, useEditor, type Editor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import * as React from "react";

import { serializeFragment } from "@/lib/notes/markdown-serialize";
import { parseMarkdownToNodes } from "@/lib/notes/markdown-parse";
import { AiSuggestionMark } from "./ai-suggestion-mark";
import { embedSlashItems } from "./embed/embed-slash-items";
import { ShikiEmbed } from "./embed/shiki-embed-node";
import { LivePreview } from "./live-preview";
import { MarkdownClipboard } from "./markdown-clipboard";
import { createSlashCommand } from "./slash-command";
import { NoteBubbleMenu, NoteToolbar } from "./note-toolbar";

/// md → エディタ doc（空でも段落 1 つは保証される）。
function docFromMarkdown(md: string) {
  return { type: "doc", content: parseMarkdownToNodes(md) };
}

export interface DraftNoteEditorProps {
  /// 再シードのトリガ。nonce が変わったときだけ本文を seed.markdown で上書きする。
  seed: { markdown: string; nonce: number };
  onChangeMarkdown: (markdown: string) => void;
  onReady?: (editor: Editor | null) => void;
}

export function DraftNoteEditor({ seed, onChangeMarkdown, onReady }: DraftNoteEditorProps) {
  const extensions = React.useMemo(
    () => [
      // 下書きはローカル履歴を持つ（Collaboration が無いため undoRedo を有効化）。
      StarterKit.configure({ link: false }),
      Link.configure({ openOnClick: false, autolink: true, protocols: ["http", "https", "mailto"] }),
      Table.configure({ resizable: false }),
      TableRow,
      TableHeader,
      TableCell,
      TaskList,
      TaskItem.configure({ nested: true }),
      Placeholder.configure({ placeholder: "入力するか、「/」でコマンドを呼び出す…" }),
      AiSuggestionMark,
      ShikiEmbed,
      MarkdownClipboard,
      LivePreview,
      createSlashCommand(() => embedSlashItems()),
    ],
    [],
  );

  const editor = useEditor(
    {
      extensions,
      immediatelyRender: false,
      content: docFromMarkdown(seed.markdown),
      editorProps: {
        attributes: { class: "note-prose focus:outline-none", "data-testid": "draft-note-editor" },
      },
      onUpdate: ({ editor }) => {
        onChangeMarkdown(serializeFragment(editor.state.doc.content));
      },
    },
    [extensions],
  );

  // 保存時の直列化のためエディタを親へ渡す。
  React.useEffect(() => {
    onReady?.(editor ?? null);
    return () => onReady?.(null);
  }, [editor, onReady]);

  // AI 流し込み/タブ切替（nonce 変化）でのみ再シードする。手編集（onUpdate）では再シードしない。
  const seededNonce = React.useRef<number>(seed.nonce);
  React.useEffect(() => {
    if (!editor) return;
    if (seededNonce.current === seed.nonce) return;
    seededNonce.current = seed.nonce;
    // emitUpdate:false で自分の setContent が onChange を再発火しないようにする（無駄書き回避）。
    editor.commands.setContent(docFromMarkdown(seed.markdown), { emitUpdate: false });
  }, [editor, seed.nonce, seed.markdown]);

  // E2E 用にエディタを公開する（本番動作には影響しない）。
  React.useEffect(() => {
    if (!editor) return;
    (window as unknown as { __shikiDraftEditor?: Editor }).__shikiDraftEditor = editor;
    return () => {
      delete (window as unknown as { __shikiDraftEditor?: Editor }).__shikiDraftEditor;
    };
  }, [editor]);

  return (
    <div className="note-editor-root min-h-[50vh]">
      {editor ? <NoteToolbar editor={editor} /> : null}
      {editor ? <NoteBubbleMenu editor={editor} /> : null}
      <EditorContent editor={editor} />
    </div>
  );
}
