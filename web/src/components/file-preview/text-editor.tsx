"use client";

/// テキスト・コードの編集（CodeMirror 6・#827）。
///
/// 閲覧（shiki）から「編集」で切り替えて使う。見た目は閲覧と揃える（同じ等幅・行の高さ・行番号の列・
/// GitHub 風の配色をライト/ダークで）。言語は拡張子から遅延で読み込む。⌘S で保存、⌘F で検索。
/// エディタの中は LF で持ち（貼り付けた行の改行コードを問わず行に分ける）、取り出すときに
/// 呼び出し側が渡した改行コード（`eol`）でつなぐ。
import * as React from "react";
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
  LanguageDescription,
  syntaxHighlighting,
} from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, type Text } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { tagHighlighter, tags as t } from "@lezer/highlight";

/// トークンに `cm-t-*` のクラスを付ける（色は globals.css でライト/ダーク別に当てる）。SQL 画面の入力欄も使う。
export const codeHighlighter = tagHighlighter([
  { tag: [t.keyword, t.modifier, t.controlKeyword, t.operatorKeyword, t.definitionKeyword], class: "cm-t-keyword" },
  { tag: [t.string, t.special(t.string), t.regexp, t.escape, t.character], class: "cm-t-string" },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment, t.meta], class: "cm-t-comment" },
  {
    tag: [t.number, t.bool, t.null, t.atom, t.constant(t.name), t.standard(t.name), t.definition(t.variableName)],
    class: "cm-t-constant",
  },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], class: "cm-t-function" },
  { tag: [t.typeName, t.className, t.namespace, t.definition(t.typeName)], class: "cm-t-type" },
  { tag: [t.attributeName, t.labelName], class: "cm-t-property" },
  { tag: [t.tagName, t.angleBracket], class: "cm-t-tag" },
  { tag: [t.heading], class: "cm-t-heading" },
  { tag: [t.link, t.url], class: "cm-t-link" },
  { tag: [t.emphasis], class: "cm-t-em" },
  { tag: [t.strong], class: "cm-t-strong" },
  { tag: [t.inserted], class: "cm-t-inserted" },
  { tag: [t.deleted], class: "cm-t-deleted" },
  { tag: [t.invalid], class: "cm-t-invalid" },
]);

/// 閲覧（`.file-code`）と同じ寸法にする。色はアプリのトークンから取る。SQL 画面の入力欄も使う。
export const codeEditorTheme = EditorView.theme({
  "&": {
    fontSize: "13px",
    backgroundColor: "transparent",
    color: "var(--foreground)",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
    lineHeight: "1.65",
    paddingBlock: "0.5rem",
  },
  ".cm-content": { caretColor: "var(--foreground)", paddingBlock: "0" },
  ".cm-line": { paddingLeft: "1rem", paddingRight: "1.5rem" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--foreground)", borderLeftWidth: "2px" },
  // 横にスクロールした本文が透けないよう、行番号の列はカードと同じ色で塗る。
  ".cm-gutters": {
    backgroundColor: "var(--card)",
    color: "color-mix(in oklab, var(--muted-foreground) 70%, transparent)",
    borderRight: "1px solid color-mix(in oklab, var(--border) 70%, transparent)",
  },
  ".cm-lineNumbers .cm-gutterElement": { paddingLeft: "1rem", paddingRight: "0.5rem", minWidth: "3ch" },
  ".cm-foldGutter .cm-gutterElement": { paddingRight: "0.25rem", cursor: "pointer" },
  ".cm-activeLine": { backgroundColor: "color-mix(in oklab, var(--foreground) 4%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--foreground)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "color-mix(in oklab, var(--ring) 22%, transparent) !important",
  },
  ".cm-selectionMatch": { backgroundColor: "color-mix(in oklab, var(--ring) 12%, transparent)" },
  "&.cm-focused .cm-matchingBracket": {
    backgroundColor: "color-mix(in oklab, var(--ring) 18%, transparent)",
    outline: "1px solid color-mix(in oklab, var(--ring) 35%, transparent)",
  },
  ".cm-searchMatch": {
    backgroundColor: "color-mix(in oklab, oklch(0.85 0.17 90) 45%, transparent)",
    borderRadius: "2px",
  },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "oklch(0.85 0.17 90 / 0.85)" },
  // 検索パネル（⌘F）をアプリの入力欄・ボタンに寄せる。
  ".cm-panels": { backgroundColor: "var(--card)", color: "var(--foreground)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-search": { padding: "0.5rem 0.75rem", display: "flex", flexWrap: "wrap", alignItems: "center", gap: "0.375rem", fontSize: "12px" },
  ".cm-search br": { display: "none" },
  ".cm-search label": { display: "inline-flex", alignItems: "center", gap: "0.25rem", color: "var(--muted-foreground)" },
  ".cm-textfield": {
    height: "1.875rem",
    borderRadius: "0.375rem",
    border: "1px solid var(--input)",
    backgroundColor: "var(--background)",
    padding: "0 0.5rem",
    fontSize: "12px",
    color: "var(--foreground)",
    margin: "0",
  },
  ".cm-textfield:focus": { outline: "2px solid var(--ring)", outlineOffset: "-1px" },
  ".cm-button": {
    height: "1.875rem",
    borderRadius: "0.375rem",
    border: "1px solid var(--border)",
    backgroundImage: "none",
    backgroundColor: "var(--card)",
    padding: "0 0.625rem",
    fontSize: "12px",
    color: "var(--foreground)",
    margin: "0",
  },
  ".cm-button:hover": { backgroundColor: "var(--accent)" },
  ".cm-search [name=close]": {
    marginLeft: "auto",
    fontSize: "16px",
    lineHeight: "1",
    padding: "0.25rem 0.5rem",
    color: "var(--muted-foreground)",
    position: "static",
  },
  ".cm-tooltip": {
    border: "1px solid var(--border)",
    backgroundColor: "var(--popover)",
    color: "var(--popover-foreground)",
    borderRadius: "0.5rem",
    overflow: "hidden",
  },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--foreground)" },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--muted)",
    border: "1px solid var(--border)",
    color: "var(--muted-foreground)",
    padding: "0 0.25rem",
  },
});

/// 日本語の UI 文言（検索パネルなど）。
const phrases = EditorState.phrases.of({
  Find: "検索",
  Replace: "置換",
  next: "次へ",
  previous: "前へ",
  all: "すべて",
  "match case": "大文字小文字を区別",
  "by word": "単語単位",
  regexp: "正規表現",
  replace: "置換",
  "replace all": "すべて置換",
  close: "閉じる",
  "current match": "現在の一致",
  "replaced $ matches": "$ 件を置換しました",
  "replaced match on line $": "$ 行目を置換しました",
  "on line": "行",
  "Go to line": "行へ移動",
  go: "移動",
  "Folded lines": "折りたたんだ行",
  "Unfolded lines": "展開した行",
  "Fold line": "行を折りたたむ",
  "Unfold line": "行を展開する",
});

export type TextEditorHandle = {
  /// 今の中身（`lineSeparator` で行をつないだ文字列）と、その時点の文書。
  snapshot: () => { text: string; doc: Text };
  /// `doc` を保存済みとみなす（保存中に打った分は未保存のまま残る）。
  markSaved: (doc: Text) => void;
  focus: () => void;
};

export const TextEditor = React.forwardRef<
  TextEditorHandle,
  {
    initial: string;
    fileName: string;
    wrap: boolean;
    eol: string;
    onDirtyChange: (dirty: boolean) => void;
    onSave: () => void;
  }
>(function TextEditor({ initial, fileName, wrap, eol, onDirtyChange, onSave }, ref) {
  const hostRef = React.useRef<HTMLDivElement>(null);
  const viewRef = React.useRef<EditorView | null>(null);
  const wrapComp = React.useRef(new Compartment());
  const langComp = React.useRef(new Compartment());
  // コールバックは最新を参照する（エディタは作り直さない）。
  const saveRef = React.useRef(onSave);
  saveRef.current = onSave;
  const eolRef = React.useRef(eol);
  eolRef.current = eol;
  const dirtyRef = React.useRef(onDirtyChange);
  dirtyRef.current = onDirtyChange;

  // 未保存かどうかの基準（開いたとき、または最後に保存したときの文書）。
  const savedDoc = React.useRef<Text | null>(null);
  const wasDirty = React.useRef(false);
  const reportDirty = React.useCallback((doc: Text) => {
    const dirty = savedDoc.current ? !doc.eq(savedDoc.current) : false;
    if (dirty !== wasDirty.current) {
      wasDirty.current = dirty;
      dirtyRef.current(dirty);
    }
  }, []);

  React.useImperativeHandle(
    ref,
    () => ({
      snapshot: () => {
        const view = viewRef.current;
        if (!view) throw new Error("editor is not ready");
        return { text: view.state.doc.toJSON().join(eolRef.current), doc: view.state.doc };
      },
      markSaved: (doc) => {
        savedDoc.current = doc;
        if (viewRef.current) reportDirty(viewRef.current.state.doc);
      },
      focus: () => viewRef.current?.focus(),
    }),
    [reportDirty],
  );

  // エディタは 1 度だけ作る（中身・改行コードは開いたときのもの）。
  React.useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: initial,
        extensions: [
          lineNumbers(),
          foldGutter({ openText: "⌄", closedText: "›" }),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          history(),
          drawSelection(),
          dropCursor(),
          EditorState.allowMultipleSelections.of(true),
          indentOnInput(),
          syntaxHighlighting(codeHighlighter),
          bracketMatching(),
          closeBrackets(),
          autocompletion({ activateOnTyping: false }),
          rectangularSelection(),
          crosshairCursor(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          keymap.of([
            {
              key: "Mod-s",
              preventDefault: true,
              run: () => {
                saveRef.current();
                return true;
              },
            },
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...searchKeymap,
            ...historyKeymap,
            ...foldKeymap,
            ...completionKeymap,
            indentWithTab,
          ]),
          wrapComp.current.of(wrap ? EditorView.lineWrapping : []),
          langComp.current.of([]),
          codeEditorTheme,
          phrases,
          EditorView.contentAttributes.of({ "aria-label": `${fileName} を編集`, spellcheck: "false" }),
          EditorView.updateListener.of((u) => {
            // 元に戻して保存済みと同じになったら未保存ではない。
            if (u.docChanged) reportDirty(u.state.doc);
          }),
        ],
      }),
    });
    savedDoc.current = view.state.doc;
    viewRef.current = view;
    view.focus();

    // 言語は拡張子から遅延で読み込む（未知の形式は色なし）。
    let active = true;
    const desc = LanguageDescription.matchFilename(languages, fileName);
    if (desc) {
      void desc
        .load()
        .then((support) => {
          if (active) view.dispatch({ effects: langComp.current.reconfigure(support) });
        })
        .catch(() => undefined);
    }
    return () => {
      active = false;
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 開いたときの中身で 1 度だけ作る
  }, []);

  React.useEffect(() => {
    viewRef.current?.dispatch({ effects: wrapComp.current.reconfigure(wrap ? EditorView.lineWrapping : []) });
  }, [wrap]);

  return <div ref={hostRef} className="text-editor min-w-0" data-testid="text-editor" />;
});
