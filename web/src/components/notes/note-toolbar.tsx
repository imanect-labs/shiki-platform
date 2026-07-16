"use client";

/// ノートエディタのツールバー（Phase 2）。
/// - `NoteToolbar`: 本文上部の静的ツールバー（常設・editable のときのみ）。
/// - `NoteBubbleMenu`: テキスト選択時に浮かぶバブルメニュー（インライン書式）。
/// slash コマンド（ブロック挿入）と併存し、書式への到達性を上げる。

import * as React from "react";
import type { Editor } from "@tiptap/react";
import { BubbleMenu } from "@tiptap/react/menus";
import {
  Bold,
  Code,
  Code2,
  Heading1,
  Heading2,
  Heading3,
  Italic,
  Link as LinkIcon,
  List,
  ListChecks,
  ListOrdered,
  Minus,
  Pilcrow,
  Quote,
  Redo2,
  Strikethrough,
  Table2,
  Sparkles,
  Undo2,
  X,
} from "lucide-react";

import { ToolbarButton, ToolbarSeparator } from "@/components/ui/floating-toolbar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

/// 選択/内容の変化時に再描画し、isActive/can の状態をボタンへ反映する。
/// ⚠️ "transaction" は BubbleMenu のプラグイン登録などでも発火し、force 再描画 →
/// 再登録 → transaction… の無限ループになる。docChanged/selectionSet に限定される
/// "update"/"selectionUpdate"（＋focus/blur）だけを購読してループを断つ。
function useEditorTick(editor: Editor | null): void {
  const [, force] = React.useReducer((x: number) => x + 1, 0);
  React.useEffect(() => {
    if (!editor) return;
    const on = () => force();
    editor.on("selectionUpdate", on);
    editor.on("update", on);
    editor.on("focus", on);
    editor.on("blur", on);
    return () => {
      editor.off("selectionUpdate", on);
      editor.off("update", on);
      editor.off("focus", on);
      editor.off("blur", on);
    };
  }, [editor]);
}

/// リンクの設定/解除。選択範囲（またはリンク全体）に適用する小ポップオーバー入力。
function LinkControl({ editor, compact }: { editor: Editor; compact?: boolean }) {
  const [open, setOpen] = React.useState(false);
  const [url, setUrl] = React.useState("");
  const active = editor.isActive("link");

  const openEditor = () => {
    setUrl((editor.getAttributes("link").href as string) ?? "");
    setOpen(true);
  };
  const apply = () => {
    const href = url.trim();
    if (href) {
      editor.chain().focus().extendMarkRange("link").setLink({ href }).run();
    } else {
      editor.chain().focus().extendMarkRange("link").unsetLink().run();
    }
    setOpen(false);
  };
  const remove = () => {
    editor.chain().focus().extendMarkRange("link").unsetLink().run();
    setOpen(false);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <ToolbarButton
          active={active}
          onClick={openEditor}
          aria-label="リンク"
          className={compact ? "size-7" : undefined}
        >
          <LinkIcon />
        </ToolbarButton>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-2">
        <div className="flex items-center gap-1.5">
          <input
            autoFocus
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                apply();
              }
              if (e.key === "Escape") setOpen(false);
            }}
            placeholder="https://…"
            className="h-8 w-full rounded-md border bg-background px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
          <button
            type="button"
            onClick={apply}
            className="h-8 shrink-0 rounded-md bg-primary px-2.5 text-xs font-medium text-primary-foreground transition-transform active:scale-95"
          >
            適用
          </button>
          {active ? (
            <button
              type="button"
              onClick={remove}
              aria-label="リンクを解除"
              className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <X className="size-4" />
            </button>
          ) : null}
        </div>
      </PopoverContent>
    </Popover>
  );
}

/// インライン書式ボタン群（bold/italic/strike/code）。バブルと静的ツールバーで共有。
function InlineMarks({ editor, compact }: { editor: Editor; compact?: boolean }) {
  const size = compact ? "size-7" : undefined;
  return (
    <>
      <ToolbarButton
        active={editor.isActive("bold")}
        onClick={() => editor.chain().focus().toggleBold().run()}
        aria-label="太字"
        className={size}
      >
        <Bold />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("italic")}
        onClick={() => editor.chain().focus().toggleItalic().run()}
        aria-label="斜体"
        className={size}
      >
        <Italic />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("strike")}
        onClick={() => editor.chain().focus().toggleStrike().run()}
        aria-label="打ち消し線"
        className={size}
      >
        <Strikethrough />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("code")}
        onClick={() => editor.chain().focus().toggleCode().run()}
        aria-label="インラインコード"
        className={size}
      >
        <Code />
      </ToolbarButton>
    </>
  );
}

/// 見出し/段落の切替（turn-into）。バブルと静的ツールバーで共有。
function BlockTypes({ editor, compact }: { editor: Editor; compact?: boolean }) {
  const size = compact ? "size-7" : undefined;
  return (
    <>
      <ToolbarButton
        active={editor.isActive("heading", { level: 1 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}
        aria-label="見出し 1"
        className={size}
      >
        <Heading1 />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("heading", { level: 2 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
        aria-label="見出し 2"
        className={size}
      >
        <Heading2 />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("blockquote")}
        onClick={() => editor.chain().focus().toggleBlockquote().run()}
        aria-label="引用"
        className={size}
      >
        <Quote />
      </ToolbarButton>
    </>
  );
}

/// テキスト選択時に浮かぶバブルメニュー（インライン書式＋turn-into＋リンク）。
export function NoteBubbleMenu({
  editor,
  onAskAi,
}: {
  editor: Editor;
  /// 選択→AI 指示（Task 11.10）。選択テキストと見出しパスを渡す（未指定ならボタン非表示）。
  onAskAi?: (selection: { text: string; headingPath: string[] }) => void;
}) {
  useEditorTick(editor);
  const askAi = () => {
    const { from, to } = editor.state.selection;
    const text = editor.state.doc.textBetween(from, to, "\n");
    if (!text.trim()) return;
    // 選択位置の直前にある見出しを親から順に辿る（locator の位置ヒント）。
    const headingPath: string[] = [];
    editor.state.doc.nodesBetween(0, from, (node) => {
      if (node.type.name === "heading") {
        const level = Number(node.attrs.level ?? 1);
        // 同位以下の見出しを置き換えつつ積む（単純なパス近似で十分）。
        while (headingPath.length >= level) headingPath.pop();
        headingPath.push(node.textContent);
      }
      return true;
    });
    onAskAi?.({ text, headingPath });
  };
  return (
    <BubbleMenu
      editor={editor}
      // 画像やコードブロック内など、テキスト選択でない場合は出さない。
      shouldShow={({ editor: e, from, to }) =>
        from !== to && !e.isActive("codeBlock") && e.isEditable}
      className="flex items-center gap-0.5 rounded-lg border bg-popover p-1 text-popover-foreground shadow-md"
    >
      <InlineMarks editor={editor} compact />
      <LinkControl editor={editor} compact />
      <ToolbarSeparator />
      <BlockTypes editor={editor} compact />
      {onAskAi ? (
        <>
          <ToolbarSeparator />
          <button
            type="button"
            onClick={askAi}
            data-testid="note-ask-ai"
            className="flex h-7 items-center gap-1 rounded px-2 text-xs font-medium text-primary transition-colors hover:bg-accent"
          >
            <Sparkles className="size-3.5" aria-hidden />
            AI に依頼
          </button>
        </>
      ) : null}
    </BubbleMenu>
  );
}

/// 本文上部の静的ツールバー（editable のときのみ表示）。
export function NoteToolbar({ editor }: { editor: Editor }) {
  useEditorTick(editor);
  const canUndo = editor.can().undo?.() ?? false;
  const canRedo = editor.can().redo?.() ?? false;

  return (
    <div
      // スクロール追従（スクロール域の上端に張り付く清潔なツールバー）。浮遊ピルではなく
      // 全幅の細いバー＋ hairline の下線。ページヘッダの直下に固定される。
      className="sticky top-0 z-20 -mx-4 mb-3 flex flex-wrap items-center gap-0.5 border-b bg-background/95 px-3 py-1.5 backdrop-blur supports-[backdrop-filter]:bg-background/75"
      data-testid="note-toolbar"
    >
      <ToolbarButton
        onClick={() => editor.chain().focus().undo().run()}
        disabled={!canUndo}
        aria-label="元に戻す"
      >
        <Undo2 />
      </ToolbarButton>
      <ToolbarButton
        onClick={() => editor.chain().focus().redo().run()}
        disabled={!canRedo}
        aria-label="やり直す"
      >
        <Redo2 />
      </ToolbarButton>
      <ToolbarSeparator />
      <ToolbarButton
        active={editor.isActive("paragraph")}
        onClick={() => editor.chain().focus().setParagraph().run()}
        aria-label="本文"
      >
        <Pilcrow />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("heading", { level: 1 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}
        aria-label="見出し 1"
      >
        <Heading1 />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("heading", { level: 2 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
        aria-label="見出し 2"
      >
        <Heading2 />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("heading", { level: 3 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}
        aria-label="見出し 3"
      >
        <Heading3 />
      </ToolbarButton>
      <ToolbarSeparator />
      <InlineMarks editor={editor} />
      <LinkControl editor={editor} />
      <ToolbarSeparator />
      <ToolbarButton
        active={editor.isActive("bulletList")}
        onClick={() => editor.chain().focus().toggleBulletList().run()}
        aria-label="箇条書き"
      >
        <List />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("orderedList")}
        onClick={() => editor.chain().focus().toggleOrderedList().run()}
        aria-label="番号付きリスト"
      >
        <ListOrdered />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("taskList")}
        onClick={() => editor.chain().focus().toggleTaskList().run()}
        aria-label="チェックリスト"
      >
        <ListChecks />
      </ToolbarButton>
      <ToolbarSeparator />
      <ToolbarButton
        active={editor.isActive("blockquote")}
        onClick={() => editor.chain().focus().toggleBlockquote().run()}
        aria-label="引用"
      >
        <Quote />
      </ToolbarButton>
      <ToolbarButton
        active={editor.isActive("codeBlock")}
        onClick={() => editor.chain().focus().toggleCodeBlock().run()}
        aria-label="コードブロック"
      >
        <Code2 />
      </ToolbarButton>
      <ToolbarButton
        onClick={() => editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()}
        aria-label="表"
      >
        <Table2 />
      </ToolbarButton>
      <ToolbarButton
        onClick={() => editor.chain().focus().setHorizontalRule().run()}
        aria-label="区切り線"
      >
        <Minus />
      </ToolbarButton>
    </div>
  );
}
