"use client";

/// スキル本文（SKILL.md）のエディタ（#919）。ノートの下書きエディタ（[`DraftNoteEditor`]）と
/// 同じ拡張（ツールバー・スラッシュ・Markdown 往復・ライブプレビュー）に、ドライブ参照
/// （[`DriveRefNode`]）を足したもの。
///
/// ドライブ参照は 3 通りで入れる: ツールバーの「ドライブから参照」・スラッシュの
/// 「ドライブから参照」・行頭か空白の直後に打った「@」。どれもピッカーを開き、選んだ項目を
/// パスの青いラベルとして挿入する（「@」はラベルに置き換わる。閉じたら「@」はそのまま残る）。
///
/// 本文は Markdown が真実源。`seed.nonce` が変わったとき（AI の流し込み・読み込み）だけ
/// 本文を置き換え、手編集は `onChangeMarkdown` で親へ書き戻す。

import { Extension, type Range } from "@tiptap/core";
import Link from "@tiptap/extension-link";
import Placeholder from "@tiptap/extension-placeholder";
import { Table } from "@tiptap/extension-table";
import { TableCell } from "@tiptap/extension-table-cell";
import { TableHeader } from "@tiptap/extension-table-header";
import { TableRow } from "@tiptap/extension-table-row";
import { TaskItem } from "@tiptap/extension-task-item";
import { TaskList } from "@tiptap/extension-task-list";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";
import { EditorContent, useEditor, type Editor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { FolderSearch } from "lucide-react";
import * as React from "react";

import { LivePreview } from "@/components/notes/live-preview";
import { MarkdownClipboard } from "@/components/notes/markdown-clipboard";
import { NOTE_PROSE_CLASS, NoteBubbleMenu, NoteToolbar } from "@/components/notes/note-toolbar";
import { createSlashCommand, type SlashItem } from "@/components/notes/slash-command";
import { YuiCodeBlock } from "@/components/notes/yui-code-block";
import { ToolbarButton } from "@/components/ui/floating-toolbar";
import { parseMarkdownToNodes } from "@/lib/notes/markdown-parse";
import { serializeFragment } from "@/lib/notes/markdown-serialize";
import type { DriveRef } from "@/lib/skills/drive-ref";
import { DriveRefNode } from "./drive-ref-node";
import { DriveRefPicker } from "./drive-ref-picker";

/// md → エディタ doc（ドライブ参照のリンクは driveRef ノードへ・`<rules>` などのタグは文字のまま）。
function docFromMarkdown(md: string) {
  return {
    type: "doc",
    content: parseMarkdownToNodes(md, { driveRefs: true, htmlAsText: true }),
  };
}

/// 参照を入れる場所。`at` は「@」から開いたときの「@」の位置（ラベルで置き換える）。
type InsertTarget = { at: number | null };

/// 参照のトリガ文字（半角と、日本語入力で入る全角）。
const AT_CHARS = new Set(["@", "＠"]);
/// アトム（ドライブ参照）を文字として読むときの置き換え（境目の判定で空白扱いにしない）。
const LEAF = "￼";

/// `pos` に「@」を打ってよい位置か（行頭か空白の直後・コードの中ではない）。
function atBoundary(view: EditorView, pos: number): boolean {
  const $pos = view.state.doc.resolve(pos);
  if ($pos.parent.type.spec.code) return false;
  const before = $pos.parent.textBetween(
    Math.max(0, $pos.parentOffset - 1),
    $pos.parentOffset,
    undefined,
    LEAF,
  );
  return before === "" || /\s/.test(before);
}

/// 行頭か空白の直後に打った「@」（全角「＠」も）でピッカーを開く（メールアドレス等の
/// 途中では開かない）。日本語入力の確定は handleTextInput を通らないので、変換の確定
/// （compositionend）でも直前の 1 文字を見る。
function atTrigger(open: (target: InsertTarget) => void) {
  return Extension.create({
    name: "driveRefAtTrigger",
    addProseMirrorPlugins() {
      return [
        new Plugin({
          key: new PluginKey("driveRefAtTrigger"),
          props: {
            handleTextInput: (view, from, _to, text) => {
              // 入力は 1 文字とは限らない（変換の確定は「規程は＠」のようにまとめて入る）ので、
              // 末尾の文字が「@」で、その直前が境目かを見る。
              const at = from + text.length - 1;
              if (!AT_CHARS.has(text.slice(-1))) return false;
              const prev = text.length > 1 ? text[text.length - 2] : null;
              if (prev !== null ? !/\s/.test(prev) : !atBoundary(view, from)) return false;
              // 「@」は既定どおり入力させ、その位置を覚えてピッカーを開く。
              window.setTimeout(() => open({ at }), 0);
              return false;
            },
            handleDOMEvents: {
              compositionend: (view) => {
                // 確定した文字が文書へ入ってから見る。
                window.setTimeout(() => {
                  const pos = view.state.selection.from;
                  if (pos < 1) return;
                  const ch = view.state.doc.textBetween(pos - 1, pos, undefined, LEAF);
                  if (AT_CHARS.has(ch) && atBoundary(view, pos - 1)) open({ at: pos - 1 });
                }, 0);
                return false;
              },
            },
          },
        }),
      ];
    },
  });
}

export interface SkillBodyEditorProps {
  seed: { markdown: string; nonce: number };
  onChangeMarkdown: (markdown: string) => void;
  onReady?: (editor: Editor | null) => void;
}

export function SkillBodyEditor({ seed, onChangeMarkdown, onReady }: SkillBodyEditorProps) {
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const targetRef = React.useRef<InsertTarget>({ at: null });
  const openPicker = React.useCallback((target: InsertTarget) => {
    targetRef.current = target;
    setPickerOpen(true);
  }, []);
  // 拡張は 1 度だけ作るので、開く処理は ref 越しに呼ぶ。
  const openRef = React.useRef(openPicker);
  openRef.current = openPicker;

  const onChangeRef = React.useRef(onChangeMarkdown);
  onChangeRef.current = onChangeMarkdown;

  const extensions = React.useMemo(() => {
    const driveSlash = (): SlashItem[] => [
      {
        title: "ドライブから参照",
        description: "ファイル・フォルダをパスのラベルで入れる",
        icon: FolderSearch,
        keywords: ["drive", "file", "folder", "ref", "sanshou", "doraibu", "fairu"],
        command: (editor, range: Range) => {
          editor.chain().focus().deleteRange(range).run();
          openRef.current({ at: null });
        },
      },
    ];
    return [
      StarterKit.configure({ link: false, codeBlock: false }),
      YuiCodeBlock,
      Link.configure({ openOnClick: false, autolink: true, protocols: ["http", "https", "mailto"] }),
      Table.configure({ resizable: false, renderWrapper: true }),
      TableRow,
      TableHeader,
      TableCell,
      TaskList,
      TaskItem.configure({ nested: true }),
      Placeholder.configure({
        placeholder:
          "スキルを使う AI への指示を書きます。「/」でコマンド、「@」でドライブのファイル・フォルダを参照…",
      }),
      DriveRefNode,
      MarkdownClipboard,
      LivePreview,
      atTrigger((t) => openRef.current(t)),
      createSlashCommand(driveSlash),
    ];
  }, []);

  const editor = useEditor(
    {
      extensions,
      immediatelyRender: false,
      content: docFromMarkdown(seed.markdown),
      editorProps: {
        attributes: {
          class: NOTE_PROSE_CLASS,
          "data-testid": "skill-body-editor",
          "aria-label": "指示文（SKILL.md 本文）",
        },
      },
      onUpdate: ({ editor }) => {
        // AI が読む指示文なので、エスケープは記法に化けるものだけにする（`doc_search` や
        // `<rules>` を書き換えない）。
        onChangeRef.current(serializeFragment(editor.state.doc.content, { literalText: true }));
      },
    },
    [extensions],
  );

  React.useEffect(() => {
    onReady?.(editor ?? null);
    return () => onReady?.(null);
  }, [editor, onReady]);

  // AI の流し込み・読み込み（nonce の変化）でだけ置き換える。手編集では置き換えない。
  const seededNonce = React.useRef<number>(seed.nonce);
  React.useEffect(() => {
    if (!editor) return;
    if (seededNonce.current === seed.nonce) return;
    seededNonce.current = seed.nonce;
    editor.commands.setContent(docFromMarkdown(seed.markdown), { emitUpdate: false });
  }, [editor, seed.nonce, seed.markdown]);

  // E2E 用にエディタを公開する（本番動作には影響しない）。
  React.useEffect(() => {
    if (!editor) return;
    (window as unknown as { __shikiSkillEditor?: Editor }).__shikiSkillEditor = editor;
    return () => {
      delete (window as unknown as { __shikiSkillEditor?: Editor }).__shikiSkillEditor;
    };
  }, [editor]);

  const insert = React.useCallback(
    (ref: DriveRef) => {
      if (!editor) return;
      const at = targetRef.current.at;
      targetRef.current = { at: null };
      const chain = editor.chain().focus();
      // 「@」（全角も）から開いたときは、その文字をラベルで置き換える（その後に打った文字は残す）。
      if (at !== null && AT_CHARS.has(editor.state.doc.textBetween(at, at + 1, undefined, LEAF))) {
        chain.deleteRange({ from: at, to: at + 1 }).setTextSelection(at);
      }
      chain.insertDriveRef(ref).run();
    },
    [editor],
  );

  return (
    <div className="note-editor-root flex min-h-[40vh] flex-col">
      {editor ? (
        <NoteToolbar
          editor={editor}
          extra={
            <ToolbarButton
              onClick={() => openPicker({ at: null })}
              aria-label="ドライブから参照"
              title="ドライブから参照（@）"
              className="h-8 w-auto min-w-8 shrink-0 gap-1.5 px-2 text-foreground/80 max-md:h-10 max-md:min-w-10"
              data-testid="skill-insert-drive-ref"
            >
              <FolderSearch />
              {/* 幅に余裕があるときだけ文字も出す（無いときはアイコンと title で示す）。 */}
              <span className="text-[13px] max-2xl:hidden">ドライブから参照</span>
            </ToolbarButton>
          }
        />
      ) : null}
      {editor ? <NoteBubbleMenu editor={editor} /> : null}
      <EditorContent editor={editor} />
      <DriveRefPicker
        open={pickerOpen}
        onOpenChange={(open) => {
          setPickerOpen(open);
          if (!open) {
            // 閉じたら本文へ戻る（選ばずに閉じた「@」はそのまま残る）。
            window.setTimeout(() => editor?.commands.focus(), 0);
          }
        }}
        onSelect={insert}
      />
    </div>
  );
}
