"use client";

/// 入力欄の本文を Markdown の WYSIWYG で書く（PromptInputTextarea の置き換え）。
///
/// 値（`PromptInput` の value）は Markdown の文字列のまま持つ。打った記法はその場で見た目に
/// 変わる（`**太字**`・`# 見出し`・`- 箇条書き`・`1. 番号`・`> 引用`・`` `コード` ``・```` ``` ````
/// のコードブロック・`~~取り消し~~`）。Ctrl/⌘+B・I なども効く。Markdown の素テキストを
/// 貼ると構造になり、コピーは Markdown になる（ノートと同じ MarkdownClipboard）。
///
/// キー操作は textarea の入力欄と同じ規則に揃える:
/// - 送信キーが Enter（既定）: Enter で送信、Shift+Enter で改行（箇条書きなら次の項目）。
///   コードブロックの中の Enter は改行（3 回続けるとブロックを抜ける）。`` ``` `` だけの行での
///   Enter はコードブロックにする。Ctrl/⌘+Enter はどこでも送信。
/// - 送信キーが Ctrl/⌘+Enter: Enter は改行、Ctrl/⌘+Enter で送信。
/// - タッチ端末（pointer: coarse）: Enter は改行（送信はボタンだけ）。
/// - 日本語入力の変換確定中の Enter は送信しない。
import { Fragment, type Node as PMNode } from "@tiptap/pm/model";
import { Selection } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";
import { getSchema } from "@tiptap/core";
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

import { MarkdownClipboard } from "@/components/notes/markdown-clipboard";
import { usePromptInput } from "@/components/prompt-kit/prompt-input";
import { useMediaQuery } from "@/hooks/use-media-query";
import { parseMarkdownToNodes } from "@/lib/notes/markdown-parse";
import { serializeFragment } from "@/lib/notes/markdown-serialize";
import { cn } from "@/lib/utils";

/// 空行。Markdown は空の段落を持てない（続く空行は 1 つに潰れる）ので、ノーブレークスペースだけの
/// 段落として送る（読む側にも表示にも空行として残る）。読み直すときは空の段落へ戻す。
const BLANK_LINE = "\u00a0";

/// 本文の構造を決める拡張（Markdown の読み込みが作るノードを全部受けられるようにする:
/// チェックリスト・表も含む）。
const SCHEMA_EXTENSIONS = [
  StarterKit.configure({ link: false }),
  // 打った語・貼った文字を自動でリンクにしない（`README.md` や `main.rs` が http://README.md の
  // リンクとして送られてしまう）。URL は文字のまま送り、表示側の GFM がリンクにする。
  Link.configure({
    openOnClick: false,
    autolink: false,
    linkOnPaste: false,
    protocols: ["http", "https", "mailto"],
  }),
  TaskList,
  TaskItem.configure({ nested: true }),
  Table.configure({ resizable: false, renderWrapper: true }),
  TableRow,
  TableHeader,
  TableCell,
];
const SCHEMA = getSchema(SCHEMA_EXTENSIONS);

/// md → エディタ doc（`<tag>` は文字のまま。HTML としては描かない）。スキーマに合わないもの
/// （読み込みが増やしたノードなど）が混じったら、本文を失わないよう行ごとの段落に落とす。
function docFromMarkdown(md: string) {
  const content = parseMarkdownToNodes(md, { htmlAsText: true }).map((node) =>
    node.type === "paragraph" &&
    node.content?.length === 1 &&
    node.content[0]?.text === BLANK_LINE
      ? { type: "paragraph" }
      : node,
  );
  const doc = { type: "doc", content };
  try {
    SCHEMA.nodeFromJSON(doc).check();
    return doc;
  } catch {
    return {
      type: "doc",
      content: md
        .split("\n")
        .map((line) => (line ? { type: "paragraph", content: [{ type: "text", text: line }] } : { type: "paragraph" })),
    };
  }
}

/// エディタ doc → md。AI が読む本文なので、エスケープは記法に化けるものだけにする
/// （`doc_search` を `doc\_search` にしない）。先頭と末尾の空行は落とし、末尾の改行は付けない
/// （textarea の値と同じ形）。間の空行は残す。
function markdownOf(editor: Editor): string {
  const { doc, schema } = editor.state;
  const blocks: PMNode[] = [];
  doc.forEach((node) => blocks.push(node));
  const blank = (node: PMNode) => node.type.name === "paragraph" && node.childCount === 0;
  let start = 0;
  let end = blocks.length;
  while (start < end && blank(blocks[start]!)) start++;
  while (end > start && blank(blocks[end - 1]!)) end--;
  const body = blocks
    .slice(start, end)
    .map((node) => (blank(node) ? schema.nodes.paragraph!.create(null, schema.text(BLANK_LINE)) : node));
  return serializeFragment(Fragment.fromArray(body), { literalText: true }).replace(/\n+$/, "");
}

/// コードブロックに変わる途中の行（`` ``` `` か `~~~` に言語名だけ）。Enter で送らずブロックにする。
const PENDING_FENCE = /^(?:```|~~~)[a-z]*$/;

/// カーソルが本文の先頭にあり、選択が無いか。キーを押した時点の DOM の選択で見る（Home の直後など、
/// エディタの状態への反映は selectionchange を待つので一拍遅れることがある）。
function caretAtStart(view: EditorView): boolean {
  const start = Selection.atStart(view.state.doc).from;
  const sel = view.dom.ownerDocument.getSelection();
  if (sel?.anchorNode && sel.isCollapsed && view.dom.contains(sel.anchorNode)) {
    try {
      return view.posAtDOM(sel.anchorNode, sel.anchorOffset) <= start;
    } catch {
      /* 対応付けられない位置はエディタの状態で判定する */
    }
  }
  const { selection } = view.state;
  return selection.empty && selection.from === start;
}

/// キー操作を呼び出し側へ渡すときの添え物。
export type PromptInputEditorKeyContext = {
  /// カーソルが本文の先頭にあり、選択が無いか（先頭の Backspace で語を外す等）。
  atStart: boolean;
};

export type PromptInputEditorProps = {
  placeholder?: string;
  autoFocus?: boolean;
  /// 送信キー。`enter`（既定）は Enter で送信・Shift+Enter で改行、`mod_enter` は Ctrl/⌘+Enter で
  /// 送信・Enter で改行。
  submitKey?: "enter" | "mod_enter";
  /// エディタより先にキーを見る（補完の上下・確定など）。`preventDefault()` したら以降は処理しない。
  onKeyDown?: (event: KeyboardEvent, context: PromptInputEditorKeyContext) => void;
  /// 本文のスクロール量が変わったとき（先頭の語を 1 行目と一緒に流す）。
  onScroll?: (scrollTop: number) => void;
  /// 1 行目の字下げ（px・先頭の語の幅）。
  firstLineIndent?: number;
  /// 入力欄の要素に付ける属性（aria-label・補完中の combobox 関連など）。
  attributes?: Record<string, string | undefined>;
  /// 本文（ProseMirror の要素）の見た目。
  className?: string;
};

export function PromptInputEditor({
  placeholder,
  autoFocus = false,
  submitKey = "enter",
  onKeyDown,
  onScroll,
  firstLineIndent,
  attributes,
  className,
}: PromptInputEditorProps) {
  const { value, setValue, maxHeight, onSubmit, disabled, textareaRef } = usePromptInput();
  // Enter のキー表示（送信 / 改行）。タッチでは Enter が改行なので「改行」を出す。
  const coarse = useMediaQuery("(pointer: coarse)");

  // 拡張・キー処理は 1 度だけ作るので、変わる値は ref 越しに読む。
  const latest = React.useRef({ onKeyDown, onSubmit, submitKey, coarse, placeholder });
  latest.current = { onKeyDown, onSubmit, submitKey, coarse, placeholder };
  // 最後にエディタから親へ渡した md。親の値がこれと違うときだけ本文を置き換える
  // （送信で空にする・書きかけを戻す・送れなかった発話を戻す）。
  const emitted = React.useRef(value);
  // Shift+Enter を「Enter の既定の動き」として流し直している間（自分の Enter 処理を飛ばす）。
  const replaying = React.useRef(false);

  const extensions = React.useMemo(
    () => [
      ...SCHEMA_EXTENSIONS,
      Placeholder.configure({ placeholder: () => latest.current.placeholder ?? "" }),
      MarkdownClipboard,
    ],
    [],
  );

  // 入力欄の要素の属性。読み上げ（role・aria-*）と、placeholder 属性（E2E とテスト支援ツールが
  // textarea と同じく引けるように）。
  const enterKeyHint = coarse || submitKey === "mod_enter" ? "enter" : "send";
  // 呼び出し側は描画ごとに新しいオブジェクトを渡すので、中身が変わったときだけ作り直す。
  const attributesKey = JSON.stringify(attributes ?? {});
  const attrs = React.useMemo(() => {
    const extra = JSON.parse(attributesKey) as Record<string, string | undefined>;
    const out: Record<string, string> = {
      role: "textbox",
      "aria-multiline": "true",
      enterkeyhint: enterKeyHint,
      class: cn("composer-editor focus:outline-none", className),
    };
    if (placeholder) {
      out.placeholder = placeholder;
      out["aria-placeholder"] = placeholder;
    }
    if (disabled) out["aria-disabled"] = "true";
    for (const [k, v] of Object.entries(extra)) if (v !== undefined) out[k] = v;
    return out;
  }, [attributesKey, className, disabled, enterKeyHint, placeholder]);

  // useEditor は描画ごとにオプションを比べ、違えば editorProps を付け直す。属性もここに入れ、
  // オブジェクトは属性が変わったときだけ作る（毎回付け直さない・後から足した属性が消えない）。
  const editorProps = React.useMemo(
    () => ({
      attributes: attrs,
      handleKeyDown: (view: EditorView, event: KeyboardEvent) => handleKey(view, event),
      handleDOMEvents: {
        // Ctrl/⌘+Enter の送信は DOM の keydown で拾う。Android の Chrome では ProseMirror が Enter の
        // keydown を handleKeyDown へ渡さない（修飾キーの無い Enter として作り直す）ため。
        keydown: (_view: EditorView, event: KeyboardEvent) => {
          if (event.key !== "Enter" || !(event.ctrlKey || event.metaKey)) return false;
          if (event.isComposing || event.keyCode === 229) return false;
          event.preventDefault();
          latest.current.onSubmit?.();
          return true;
        },
      },
    }),
    [attrs],
  );
  // 初期の本文（以降の置き換えは下の effect。描画ごとに作ると useEditor がオプションの変化とみなす）。
  const [initialContent] = React.useState(() => docFromMarkdown(value));

  const editor = useEditor({
    extensions,
    immediatelyRender: false,
    content: initialContent,
    editable: !disabled,
    editorProps,
    onUpdate: ({ editor }) => {
      const md = markdownOf(editor);
      emitted.current = md;
      setValue(md);
    },
  });

  /// キー操作（editorProps から呼ぶ。変わる値は ref 越しに読むので、作り直さなくてよい）。
  function handleKey(view: EditorView, event: KeyboardEvent): boolean {
    if (replaying.current) return false;
    const { onKeyDown, onSubmit, submitKey, coarse } = latest.current;
    const { selection } = view.state;
    onKeyDown?.(event, { atStart: caretAtStart(view) });
    if (event.defaultPrevented) return true;
    // IME 変換確定中（日本語入力）の Enter は確定に使わせる。Safari は確定の Enter で
    // isComposing を落とすことがあるので keyCode 229 も見る。
    if (event.key !== "Enter" || event.isComposing || event.keyCode === 229) return false;
    // Ctrl/⌘+Enter（どの送信キー・タッチ端末の外付けキーボードでも送る）は handleDOMEvents で済ませている。
    if (event.ctrlKey || event.metaKey) return true;
    // 改行にする側（Ctrl/⌘+Enter で送る設定・タッチ端末）は既定の Enter のまま。
    if (submitKey === "mod_enter" || coarse) return false;
    if (event.shiftKey) {
      // 既定の Shift+Enter は段落内の強制改行だが、ここでは Enter と同じ動き（新しい段落・
      // 次の項目・コード内の改行）にする。段落を分けておくと、続く行の `- ` などの記法が効く。
      // 修飾キーの無い Enter として各拡張の Enter 処理へ流し直す（トランザクションはそのまま
      // 反映されるので、コードブロックを抜けたときのカーソル移動も保たれる）。
      const enter = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
      replaying.current = true;
      try {
        view.someProp("handleKeyDown", (f) => f(view, enter));
      } finally {
        replaying.current = false;
      }
      return true;
    }
    const { $from } = selection;
    if ($from.parent.type.spec.code) return false;
    if (selection.empty && PENDING_FENCE.test($from.parent.textContent)) return false;
    onSubmit?.();
    return true;
  }

  // 親が値を変えたとき（送信後の空・書きかけの復元）だけ本文を置き換える。打鍵ごとには置き換えない。
  React.useEffect(() => {
    if (!editor || value === emitted.current) return;
    emitted.current = value;
    editor.commands.setContent(docFromMarkdown(value), { emitUpdate: false });
  }, [editor, value]);

  React.useEffect(() => {
    editor?.setEditable(!disabled, false);
  }, [editor, disabled]);

  // プレースホルダは状態（エージェントモード・コマンド）で変わるので、変わったら描き直す。
  React.useEffect(() => {
    if (editor && !editor.isDestroyed) editor.view.dispatch(editor.state.tr);
  }, [editor, placeholder]);

  // 枠の余白を押したとき・先頭の語を外したときにフォーカスを移す先。
  React.useEffect(() => {
    if (!editor) return;
    textareaRef.current = editor.view.dom;
    return () => {
      if (textareaRef.current === editor.view.dom) textareaRef.current = null;
    };
  }, [editor, textareaRef]);


  // マウスの端末だけ開いた瞬間にフォーカスする（タッチではソフトキーボードを出さない）。
  React.useEffect(() => {
    if (!editor || !autoFocus) return;
    // 利用者が既に別の入力欄で打っているとき（会話を開いた直後にサイドバーで名前を変える等）は
    // 奪わない（奪うとその欄の blur で入力が確定・破棄される）。
    const active = document.activeElement;
    const typing =
      active instanceof HTMLElement &&
      active !== editor.view.dom &&
      (active.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(active.tagName));
    if (typing) return;
    if (window.matchMedia("(pointer: fine)").matches) {
      editor.commands.focus("end", { scrollIntoView: false });
    }
    // エディタができたときだけ（スレッドを開いたとき・ホーム）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor]);

  return (
    <div
      className="min-w-0 overflow-y-auto overscroll-contain"
      style={{
        maxHeight: typeof maxHeight === "number" ? `${maxHeight}px` : maxHeight,
        ["--composer-indent" as string]: firstLineIndent ? `${firstLineIndent}px` : undefined,
      }}
      onScroll={(e) => onScroll?.(e.currentTarget.scrollTop)}
    >
      {editor ? (
        <EditorContent editor={editor} />
      ) : (
        // エディタができるまで（初回描画）は同じ高さの空の枠を置き、レイアウトを跳ねさせない。
        <div aria-hidden className={cn("composer-editor", className)}>
          <p className="truncate text-muted-foreground/70">{placeholder}</p>
        </div>
      )}
    </div>
  );
}
