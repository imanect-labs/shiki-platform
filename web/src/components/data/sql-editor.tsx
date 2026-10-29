"use client";

/// SQL 画面の入力欄（#978）。ファイルの編集（`file-preview/text-editor.tsx`）と同じ CodeMirror 6 の見た目で、
/// SQL の色分け・閲覧者に見えるテーブルと列の補完・⌘/Ctrl+Enter の実行を持つ。
///
/// 中身は親の `value` が正（下書きの復元・例・候補の置き換えは親が `value` を変える）。打った分は `onChange` で返す。
/// 位置は文字列の添字（UTF-16）で、`value` の添字とそのまま対応する。

import {
  acceptCompletion,
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  closeCompletion,
  completionKeymap,
  type CompletionSource,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { bracketMatching, syntaxHighlighting } from "@codemirror/language";
import { sql, StandardSQL } from "@codemirror/lang-sql";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import {
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  placeholder as placeholderExt,
} from "@codemirror/view";
import * as React from "react";

import { sqlIdent } from "@/components/data/sql-reference";
import { codeEditorTheme, codeHighlighter } from "@/components/file-preview/text-editor";
import { useMediaQuery } from "@/hooks/use-media-query";
import { cn } from "@/lib/utils";

export type SqlEditorHandle = {
  focus: () => void;
  /// 今の選択範囲（[from, to)）。
  selection: () => { from: number; to: number };
  /// [from, to) を選んでフォーカスする（拒否の位置の名前を選ぶ）。
  select: (from: number, to: number) => void;
};

/// 入力欄の高さに合わせる（行の高さ・余白はファイルの編集と同じ）。
const sqlTheme = EditorView.theme({
  "&": { height: "100%" },
  ".cm-scroller": { overflow: "auto" },
  ".cm-placeholder": { color: "color-mix(in oklab, var(--muted-foreground) 70%, transparent)" },
  ".cm-tooltip-autocomplete > ul": { fontFamily: "var(--font-mono, ui-monospace, monospace)", fontSize: "12.5px" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul > li": { padding: "2px 10px", lineHeight: "1.6" },
  ".cm-completionMatchedText": { textDecoration: "none", fontWeight: "600", color: "var(--primary)" },
  ".cm-completionDetail": { marginLeft: "0.75rem", fontStyle: "normal", color: "var(--muted-foreground)" },
});

/// 日本語など ASCII 以外を含む名前の補完（lang-sql の補完は ASCII の語だけを対象にするため、足りない分を補う）。
function unicodeNames(schema: Record<string, string[]>): CompletionSource {
  const tables = Object.keys(schema);
  const columns = [...new Set(Object.values(schema).flat())];
  const options = [
    ...tables.map((t) => ({ label: t, type: "type", apply: sqlIdent(t) })),
    ...columns.map((c) => ({ label: c, type: "property", apply: sqlIdent(c) })),
  ];
  return (ctx) => {
    const word = ctx.matchBefore(/[\p{L}\p{N}_]+/u);
    if (!word || !/[^\x00-\x7f]/.test(word.text)) return null;
    return { from: word.from, options, validFor: /^[\p{L}\p{N}_]*$/u };
  };
}

export const SqlEditor = React.forwardRef<
  SqlEditorHandle,
  {
    value: string;
    onChange: (value: string) => void;
    onRun: () => void;
    /// 補完に使うテーブル → 列（閲覧者に見えるものだけ）。
    schema: Record<string, string[]>;
    placeholder: string;
    invalid?: boolean;
    className?: string;
  }
>(function SqlEditor({ value, onChange, onRun, schema, placeholder, invalid, className }, ref) {
  const hostRef = React.useRef<HTMLDivElement>(null);
  const viewRef = React.useRef<EditorView | null>(null);
  const langComp = React.useRef(new Compartment());
  const wrapComp = React.useRef(new Compartment());
  const placeholderComp = React.useRef(new Compartment());
  // コールバックは最新を参照する（エディタは作り直さない）。
  const changeRef = React.useRef(onChange);
  const runRef = React.useRef(onRun);
  React.useEffect(() => {
    changeRef.current = onChange;
    runRef.current = onRun;
  }, [onChange, onRun]);
  // 狭い画面は折り返す（横スクロールで書きにくい）。
  const narrow = useMediaQuery("(max-width: 767.98px)");

  React.useImperativeHandle(
    ref,
    () => ({
      focus: () => viewRef.current?.focus(),
      selection: () => {
        const sel = viewRef.current?.state.selection.main;
        return { from: sel?.from ?? value.length, to: sel?.to ?? value.length };
      },
      select: (from, to) => {
        const view = viewRef.current;
        if (!view) return;
        const len = view.state.doc.length;
        view.dispatch({ selection: { anchor: Math.min(from, len), head: Math.min(to, len) }, scrollIntoView: true });
        view.focus();
      },
    }),
    [value.length],
  );

  // エディタは 1 度だけ作る。
  React.useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          history(),
          drawSelection(),
          syntaxHighlighting(codeHighlighter),
          bracketMatching(),
          closeBrackets(),
          autocompletion({ icons: false }),
          highlightActiveLine(),
          // ⌘/Ctrl+Enter は実行（既定の「空行を入れる」より先に取る）。Tab は補完の確定。
          Prec.highest(
            keymap.of([
              {
                key: "Mod-Enter",
                preventDefault: true,
                run: (view) => {
                  closeCompletion(view);
                  runRef.current();
                  return true;
                },
              },
              { key: "Tab", run: acceptCompletion },
            ]),
          ),
          keymap.of([...closeBracketsKeymap, ...completionKeymap, ...defaultKeymap, ...historyKeymap]),
          langComp.current.of([]),
          wrapComp.current.of([]),
          placeholderComp.current.of(placeholderExt(placeholder)),
          codeEditorTheme,
          sqlTheme,
          EditorView.contentAttributes.of({
            "aria-label": "SQL",
            spellcheck: "false",
            autocapitalize: "off",
            autocorrect: "off",
          }),
          // 入力欄を離れたら（実行・参照一覧を押した）補完の一覧を閉じる。
          EditorView.domEventHandlers({
            blur: (_e, view) => {
              closeCompletion(view);
            },
          }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) changeRef.current(u.state.doc.toString());
          }),
        ],
      }),
    });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 開いたときの中身で 1 度だけ作る（以降は value の同期で反映）
  }, []);

  // 親が変えた中身（下書きの復元・例・候補の置き換え）を反映する。打った分は同じ値なので何もしない。
  React.useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const cur = view.state.doc.toString();
    if (cur === value) return;
    // 変わった範囲だけ置き換え、カーソルはその後ろに置く（候補・参照一覧の挿入がそのまま続けて書ける）。
    let start = 0;
    while (start < cur.length && start < value.length && cur[start] === value[start]) start++;
    let endCur = cur.length;
    let endNew = value.length;
    while (endCur > start && endNew > start && cur[endCur - 1] === value[endNew - 1]) {
      endCur--;
      endNew--;
    }
    view.dispatch({
      changes: { from: start, to: endCur, insert: value.slice(start, endNew) },
      selection: { anchor: endNew },
      scrollIntoView: true,
    });
  }, [value]);

  React.useEffect(() => {
    const lang = sql({ dialect: StandardSQL, schema, upperCaseKeywords: true });
    viewRef.current?.dispatch({
      effects: langComp.current.reconfigure([lang, lang.language.data.of({ autocomplete: unicodeNames(schema) })]),
    });
  }, [schema]);
  React.useEffect(() => {
    viewRef.current?.dispatch({ effects: wrapComp.current.reconfigure(narrow ? EditorView.lineWrapping : []) });
  }, [narrow]);
  React.useEffect(() => {
    viewRef.current?.dispatch({ effects: placeholderComp.current.reconfigure(placeholderExt(placeholder)) });
  }, [placeholder]);

  return (
    <div
      ref={hostRef}
      className={cn(
        "text-editor h-full min-w-0 max-md:[&_.cm-editor]:text-base",
        invalid && "[&_.cm-gutters]:border-r-destructive/40",
        className,
      )}
      data-testid="sql-editor"
    />
  );
});
