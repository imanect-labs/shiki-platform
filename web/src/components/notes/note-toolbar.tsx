"use client";

/// ノートエディタのツールバー（Phase 2）。
/// - `NoteToolbar`: 本文上部の静的ツールバー（常設・editable のときのみ）。スマホでは 1 段の
///   横スクロールにし、低頻度（見出し 3・区切り線・表）は右端の「＋」へ寄せる。タッチで本文を
///   編集している間（キーボード表示中）は、上のバーの代わりにキーボード直上の補助バーを出す。
/// - `NoteBubbleMenu`: テキスト選択時に浮かぶバブルメニュー（インライン書式）。タッチ端末では
///   出さない（OS の選択メニューと競合し、フォーカスが外れてキーボードがちらつくため）。
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

  Undo2,
  Check,
  Plus,
  X,
} from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ToolbarButton, ToolbarSeparator } from "@/components/ui/floating-toolbar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";

/// 本文（contenteditable）のクラス。スマホは 16px（iOS のフォーカス時ズーム対策）、表は
/// 横スクロールの箱に入れ、狭い幅でも列が潰れないよう最小幅を持たせる。
export const NOTE_PROSE_CLASS = cn(
  "note-prose focus:outline-none max-md:text-base",
  "[&_.tableWrapper]:my-3 [&_.tableWrapper]:overflow-x-auto [&_.tableWrapper]:overscroll-x-contain [&_.tableWrapper_table]:my-0",
  "max-md:[&_.tableWrapper_td]:min-w-28 max-md:[&_.tableWrapper_th]:min-w-28",
);

/// スマホでのツールバーボタンの大きさ（タップ 40px）。PC は従来の 32px。
const TB = "shrink-0 max-md:size-10 max-md:[&_svg]:size-[18px]";

/// タッチ端末で本文にフォーカスがある（＝キーボードが出ている）か。ツールバーのボタンを押した
/// 瞬間の blur で補助バーが消えないよう、blur は少し遅らせて反映する。
function useTouchEditing(editor: Editor): boolean {
  const touchNarrow = useMediaQuery("(pointer: coarse) and (max-width: 767.98px)");
  const [focused, setFocused] = React.useState(false);
  React.useEffect(() => {
    let timer: number | undefined;
    const onFocus = () => {
      window.clearTimeout(timer);
      setFocused(true);
    };
    const onBlur = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setFocused(false), 180);
    };
    setFocused(editor.isFocused);
    editor.on("focus", onFocus);
    editor.on("blur", onBlur);
    return () => {
      window.clearTimeout(timer);
      editor.off("focus", onFocus);
      editor.off("blur", onBlur);
    };
  }, [editor]);
  return touchNarrow && focused && editor.isEditable;
}

/// ソフトウェアキーボードの高さ（visualViewport が layout viewport の下端から食い込む量）。
/// iOS は layout viewport を縮めないため、fixed の下端をこの分だけ持ち上げる。
function useKeyboardInset(active: boolean): number {
  const [inset, setInset] = React.useState(0);
  React.useEffect(() => {
    const vv = typeof window !== "undefined" ? window.visualViewport : null;
    if (!active || !vv) {
      setInset(0);
      return;
    }
    const update = () =>
      setInset(Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop)));
    update();
    vv.addEventListener("resize", update);
    vv.addEventListener("scroll", update);
    return () => {
      vv.removeEventListener("resize", update);
      vv.removeEventListener("scroll", update);
    };
  }, [active]);
  return inset;
}

/// ボタン押下でエディタのフォーカスを奪わない（キーボードを閉じない・選択を保つ）。
const keepFocus = (e: React.MouseEvent) => e.preventDefault();

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
function LinkControl({ editor, className }: { editor: Editor; className?: string }) {
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
          className={className}
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
            type="url"
            inputMode="url"
            enterKeyHint="done"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            className="h-8 w-full rounded-md border bg-background px-2 text-base outline-none focus-visible:ring-2 focus-visible:ring-ring max-md:h-10 md:text-sm"
          />
          <button
            type="button"
            onClick={apply}
            className="h-8 shrink-0 rounded-md bg-primary px-2.5 text-xs font-medium text-primary-foreground transition-transform active:scale-95 max-md:h-10 max-md:px-3.5 max-md:text-sm"
          >
            適用
          </button>
          {active ? (
            <button
              type="button"
              onClick={remove}
              aria-label="リンクを解除"
              className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground max-md:size-10"
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
function InlineMarks({ editor, className: size }: { editor: Editor; className?: string }) {
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
function BlockTypes({ editor, className: size }: { editor: Editor; className?: string }) {
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

/// 現在の選択テキストと見出しパス（選択→AI の locator ヒント）を読む。選択が空なら null。
export function readNoteSelection(editor: Editor): { text: string; headingPath: string[] } | null {
  const { from, to } = editor.state.selection;
  const text = editor.state.doc.textBetween(from, to, "\n");
  if (!text.trim()) return null;
  // 選択位置の直前にある見出しを親から順に辿る（locator の位置ヒント）。
  const headingPath: string[] = [];
  editor.state.doc.nodesBetween(0, from, (node) => {
    if (node.type.name === "heading") {
      const level = Number(node.attrs.level ?? 1);
      while (headingPath.length >= level) headingPath.pop();
      headingPath.push(node.textContent);
    }
    return true;
  });
  return { text, headingPath };
}

/// テキスト選択時に浮かぶバブルメニュー（インライン書式＋turn-into＋リンク）。
export function NoteBubbleMenu({ editor }: { editor: Editor }) {
  useEditorTick(editor);
  // タッチ端末では出さない（書式は上のツールバー／キーボード直上の補助バーに集約）。
  const coarse = useMediaQuery("(pointer: coarse)");
  if (coarse) return null;
  return (
    <BubbleMenu
      editor={editor}
      // 既定の上配置は文書先頭の選択で固定ツールバーと重なって両方読めなくなる（実測）。
      // 下配置なら常設 UI と衝突しない（viewport 端では floating-ui が自動で反転する）。
      options={{ placement: "bottom" }}
      // 画像やコードブロック内など、テキスト選択でない場合は出さない。
      shouldShow={({ editor: e, from, to }) =>
        from !== to && !e.isActive("codeBlock") && e.isEditable}
      // z-30: 静的ツールバー（sticky・z-20）より前面に出す（文頭選択でボタンが覆われる不具合対策）。
      className="z-30 flex items-center gap-0.5 rounded-lg border bg-popover p-1 text-popover-foreground shadow-md"
    >
      {/* ボタン押下でフォーカスと選択を保つ（押した瞬間に選択が外れて書式が当たらない対策）。 */}
      <div className="flex items-center gap-0.5" onMouseDown={keepFocus}>
        <InlineMarks editor={editor} className="size-7" />
        <LinkControl editor={editor} className="size-7" />
        <ToolbarSeparator className="shrink-0" />
        <BlockTypes editor={editor} className="size-7" />
      </div>
    </BubbleMenu>
  );
}

/// 低頻度のブロック挿入（スマホのツールバー右端の「＋」）。
function InsertMenu({ editor }: { editor: Editor }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <ToolbarButton aria-label="その他の挿入" className={TB}>
          <Plus />
        </ToolbarButton>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" collisionPadding={12} className="w-48">
        <DropdownMenuItem
          className="gap-2.5"
          onSelect={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}
        >
          <Heading3 className="size-4 text-muted-foreground" />
          見出し 3
        </DropdownMenuItem>
        <DropdownMenuItem
          className="gap-2.5"
          onSelect={() =>
            editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()
          }
        >
          <Table2 className="size-4 text-muted-foreground" />
          表
        </DropdownMenuItem>
        <DropdownMenuItem
          className="gap-2.5"
          onSelect={() => editor.chain().focus().setHorizontalRule().run()}
        >
          <Minus className="size-4 text-muted-foreground" />
          区切り線
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// 本文上部の静的ツールバー（editable のときのみ表示）。
export function NoteToolbar({ editor }: { editor: Editor }) {
  useEditorTick(editor);
  const touchEditing = useTouchEditing(editor);
  const canUndo = editor.can().undo?.() ?? false;
  const canRedo = editor.can().redo?.() ?? false;

  // タッチで編集中は、上のバーの代わりにキーボード直上の補助バーを出す。上のバーは外さずに
  // 見えなく・操作できなくするだけにして場所を残す（外すと本文が約 64px 上へずれ、指の下の
  // 行が入れ替わる）。
  return (
    <>
      {touchEditing ? (
        <KeyboardAccessoryBar editor={editor} canUndo={canUndo} canRedo={canRedo} />
      ) : null}
      <div
        // スクロール追従（スクロール域の上端に張り付く清潔なツールバー）。浮遊ピルではなく
        // 全幅の細いバー＋ hairline の下線。ページヘッダの直下に固定される。
        className={cn(
          "sticky top-0 z-20 -mx-4 mb-3 flex items-center border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/75",
          touchEditing && "invisible",
        )}
        data-testid={touchEditing ? undefined : "note-toolbar"}
        aria-hidden={touchEditing || undefined}
        inert={touchEditing}
      >
        {/* PC は折り返し、スマホは 1 段の横スクロール（右端は「＋」の手前でフェード）。 */}
        <div className="relative min-w-0 flex-1">
          <div className="flex items-center gap-0.5 px-3 py-1.5 md:flex-wrap max-md:overflow-x-auto max-md:overscroll-x-contain max-md:pr-6 max-md:[scrollbar-width:none]">
            <ToolbarButton
              onClick={() => editor.chain().focus().undo().run()}
              disabled={!canUndo}
              aria-label="元に戻す"
              className={TB}
            >
              <Undo2 />
            </ToolbarButton>
            <ToolbarButton
              onClick={() => editor.chain().focus().redo().run()}
              disabled={!canRedo}
              aria-label="やり直す"
              className={TB}
            >
              <Redo2 />
            </ToolbarButton>
            <ToolbarSeparator className="shrink-0" />
            <ToolbarButton
              active={editor.isActive("paragraph")}
              onClick={() => editor.chain().focus().setParagraph().run()}
              aria-label="本文"
              className={TB}
            >
              <Pilcrow />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("heading", { level: 1 })}
              onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}
              aria-label="見出し 1"
              className={TB}
            >
              <Heading1 />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("heading", { level: 2 })}
              onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
              aria-label="見出し 2"
              className={TB}
            >
              <Heading2 />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("heading", { level: 3 })}
              onClick={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}
              aria-label="見出し 3"
              className="max-md:hidden"
            >
              <Heading3 />
            </ToolbarButton>
            <ToolbarSeparator className="shrink-0" />
            <InlineMarks editor={editor} className={TB} />
            <LinkControl editor={editor} className={TB} />
            <ToolbarSeparator className="shrink-0" />
            <ToolbarButton
              active={editor.isActive("bulletList")}
              onClick={() => editor.chain().focus().toggleBulletList().run()}
              aria-label="箇条書き"
              className={TB}
            >
              <List />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("orderedList")}
              onClick={() => editor.chain().focus().toggleOrderedList().run()}
              aria-label="番号付きリスト"
              className={TB}
            >
              <ListOrdered />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("taskList")}
              onClick={() => editor.chain().focus().toggleTaskList().run()}
              aria-label="チェックリスト"
              className={TB}
            >
              <ListChecks />
            </ToolbarButton>
            <ToolbarSeparator className="shrink-0" />
            <ToolbarButton
              active={editor.isActive("blockquote")}
              onClick={() => editor.chain().focus().toggleBlockquote().run()}
              aria-label="引用"
              className={TB}
            >
              <Quote />
            </ToolbarButton>
            <ToolbarButton
              active={editor.isActive("codeBlock")}
              onClick={() => editor.chain().focus().toggleCodeBlock().run()}
              aria-label="コードブロック"
              className={TB}
            >
              <Code2 />
            </ToolbarButton>
            <ToolbarButton
              onClick={() =>
                editor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()
              }
              aria-label="表"
              className="max-md:hidden"
            >
              <Table2 />
            </ToolbarButton>
            <ToolbarButton
              onClick={() => editor.chain().focus().setHorizontalRule().run()}
              aria-label="区切り線"
              className="max-md:hidden"
            >
              <Minus />
            </ToolbarButton>
          </div>
          <div
            aria-hidden
            className="pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l from-background to-transparent md:hidden"
          />
        </div>
        <div className="shrink-0 pr-2 md:hidden">
          <InsertMenu editor={editor} />
        </div>
      </div>
    </>
  );
}

/// タッチで編集中（キーボード表示中）に、キーボードの直上へ固定する補助バー。
/// 押下でフォーカスを奪わない（keepFocus）ので、書式を当ててもキーボードは閉じない。
function KeyboardAccessoryBar({
  editor,
  canUndo,
  canRedo,
}: {
  editor: Editor;
  canUndo: boolean;
  canRedo: boolean;
}) {
  const inset = useKeyboardInset(true);
  return (
    <div
      role="toolbar"
      aria-label="書式"
      data-testid="note-keyboard-bar"
      onMouseDown={keepFocus}
      style={{ bottom: inset }}
      className={cn(
        "fixed inset-x-0 z-40 flex items-center border-t bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/85",
        inset === 0 && "pb-[env(safe-area-inset-bottom)]",
      )}
    >
      <div className="relative min-w-0 flex-1">
        <div className="flex items-center gap-0.5 overflow-x-auto overscroll-x-contain px-2 py-1 pr-6 [scrollbar-width:none]">
          <ToolbarButton
            onClick={() => editor.chain().focus().undo().run()}
            disabled={!canUndo}
            aria-label="元に戻す"
            className={TB}
          >
            <Undo2 />
          </ToolbarButton>
          <ToolbarButton
            onClick={() => editor.chain().focus().redo().run()}
            disabled={!canRedo}
            aria-label="やり直す"
            className={TB}
          >
            <Redo2 />
          </ToolbarButton>
          <ToolbarSeparator className="shrink-0" />
          <InlineMarks editor={editor} className={TB} />
          <ToolbarSeparator className="shrink-0" />
          <BlockTypes editor={editor} className={TB} />
          <ToolbarSeparator className="shrink-0" />
          <ToolbarButton
            active={editor.isActive("bulletList")}
            onClick={() => editor.chain().focus().toggleBulletList().run()}
            aria-label="箇条書き"
            className={TB}
          >
            <List />
          </ToolbarButton>
          <ToolbarButton
            active={editor.isActive("orderedList")}
            onClick={() => editor.chain().focus().toggleOrderedList().run()}
            aria-label="番号付きリスト"
            className={TB}
          >
            <ListOrdered />
          </ToolbarButton>
          <ToolbarButton
            active={editor.isActive("taskList")}
            onClick={() => editor.chain().focus().toggleTaskList().run()}
            aria-label="チェックリスト"
            className={TB}
          >
            <ListChecks />
          </ToolbarButton>
          <ToolbarButton
            active={editor.isActive("codeBlock")}
            onClick={() => editor.chain().focus().toggleCodeBlock().run()}
            aria-label="コードブロック"
            className={TB}
          >
            <Code2 />
          </ToolbarButton>
        </div>
        <div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l from-background to-transparent"
        />
      </div>
      {/* キーボードを閉じる（本文のフォーカスを外す）。 */}
      <button
        type="button"
        onClick={() => editor.commands.blur()}
        aria-label="キーボードを閉じる"
        className="mr-1.5 flex h-10 shrink-0 items-center gap-1 rounded-full px-3 text-sm font-medium text-primary transition-colors active:bg-accent"
      >
        <Check className="size-4" aria-hidden />
        完了
      </button>
    </div>
  );
}
