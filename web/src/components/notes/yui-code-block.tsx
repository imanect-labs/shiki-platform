"use client";

/// ノートの ```ui コードブロック（Yui・#530）を図として描く。
///
/// - **保存形式は変えない**: 文書上は通常の codeBlock（language = ui / yui / ui:bar）のまま。
///   Yjs・Markdown の往復・サーバ（collab の md_parse / yjs_map）は何も知らなくてよい。
///   図は NodeView の表示だけで、文書を書き換えない（共同編集セーフ）。
/// - Yui 以外の言語のコードブロックは NodeView を作らず、既定の `<pre><code>` 描画のまま。
/// - 既定は図を見せる。カーソルがブロックの中に入ったとき・空のとき・図にできないときは記述
///   （NodeViewContent＝通常のコード編集）を見せ、編集に合わせて図を描き直す。
/// - 読み取り専用のエディタ（閲覧者・印刷ビュー）は図だけを見せる（描けなければ記述と一言）。
///
/// 信頼境界: 記述はノートの編集者（AI 編集を含む）が書いた任意の文字列。Yui は文字と数値だけを
/// SVG の React 要素として描き、HTML・URL・スクリプトを持たない（dangerouslySetInnerHTML 無し）。

import { CodeBlock } from "@tiptap/extension-code-block";
import type { Node as PMNode } from "@tiptap/pm/model";
import { TextSelection } from "@tiptap/pm/state";
import {
  NodeViewContent,
  NodeViewWrapper,
  ReactNodeViewRenderer,
  type Editor,
  type NodeViewProps,
} from "@tiptap/react";
import { isEmpty, parseBlock, resolveKind, type Diagnostic, type Kind } from "@imanect/yui";
import { YuiFigure, isYuiLanguage } from "@imanect/yui/react";
import { BarChart3, Code2 } from "lucide-react";
import * as React from "react";

import { renderFigureTable } from "@/components/prompt-kit/markdown";
import { ToolbarButton, UiFigure } from "@/components/prompt-kit/ui-figure";
import { cn } from "@/lib/utils";

/// codeBlock の language 属性が Yui か（`ui` / `yui` / `ui:bar` / `ui bar`）。
export function isYuiCodeLanguage(language: unknown): boolean {
  return typeof language === "string" && isYuiLanguage(`language-${language}`);
}

/// language 属性に書かれた図の種類（```ui:bar / ```ui bar）。
export function yuiKindOf(language: unknown): Kind | undefined {
  if (typeof language !== "string") return undefined;
  const m = /^(?:ui|yui)(?:\s+|:)(\S+)/i.exec(language.trim());
  return m ? resolveKind(m[1]) : undefined;
}

function isYuiNode(node: PMNode): boolean {
  return isYuiCodeLanguage(node.attrs.language);
}

/// 図の状態。ok 以外は（編集できるなら）記述を見せる。
type FigureState =
  | { kind: "empty" }
  | { kind: "error"; diagnostic?: Diagnostic }
  | { kind: "incomplete" }
  | { kind: "ok" };

function lineMessage(d: Diagnostic): string {
  return d.line > 0 ? `${d.line} 行目: ${d.message}` : d.message;
}

/// エディタが編集可能か・カーソル（選択）がこのブロックの中にあってフォーカスがあるか。
/// 読み込み直後の既定の選択（文書先頭）で先頭の図が記述表示にならないよう、フォーカスも見る。
/// NodeView はノードが変わったときしか描き直されないので、エディタのイベントを購読する。
function useEditorFlags(
  editor: Editor,
  getPos: () => number | undefined,
): { editable: boolean; inside: boolean } {
  const compute = React.useCallback(
    (focused: boolean) => {
      const editable = editor.isEditable;
      const pos = getPos();
      const node = typeof pos === "number" ? editor.state.doc.nodeAt(pos) : null;
      if (!focused || !editable || typeof pos !== "number" || !node) {
        return { editable, inside: false };
      }
      const { from, to } = editor.state.selection;
      return { editable, inside: from > pos && to < pos + node.nodeSize };
    },
    [editor, getPos],
  );
  const [flags, setFlags] = React.useState(() => compute(editor.isFocused));
  React.useEffect(() => {
    let focused = editor.isFocused;
    const update = () => {
      const next = compute(focused);
      setFlags((prev) =>
        prev.editable === next.editable && prev.inside === next.inside ? prev : next,
      );
    };
    const onFocus = () => {
      focused = true;
      update();
    };
    const onBlur = () => {
      focused = false;
      update();
    };
    update();
    // setEditable は "update" を出す（閲覧⇄編集の切り替えに追従する）。
    editor.on("selectionUpdate", update);
    editor.on("update", update);
    editor.on("focus", onFocus);
    editor.on("blur", onBlur);
    return () => {
      editor.off("selectionUpdate", update);
      editor.off("update", update);
      editor.off("focus", onFocus);
      editor.off("blur", onBlur);
    };
  }, [editor, compute]);
  return flags;
}

function YuiCodeBlockView({ node, editor, getPos }: NodeViewProps) {
  const source = node.textContent;
  const kind = yuiKindOf(node.attrs.language);
  const { editable, inside } = useEditorFlags(editor, getPos);

  const result = React.useMemo(
    () => parseBlock(source, kind ? { final: true, kind } : { final: true }),
    [source, kind],
  );
  const state: FigureState = React.useMemo(() => {
    if (source.trim() === "") return { kind: "empty" };
    const error = result.diagnostics.find((d) => d.severity === "error");
    if (!result.figure || error) return { kind: "error", diagnostic: error };
    if (isEmpty(result.figure)) return { kind: "incomplete" };
    return { kind: "ok" };
  }, [source, result]);

  // 利用者が選んだ表示。カーソルの出入り（遷移）でだけ自動で切り替え、それ以外は選択を尊重する。
  const [mode, setMode] = React.useState<"figure" | "source">(() =>
    inside ? "source" : "figure",
  );
  const prevInside = React.useRef(inside);
  // 記述から Tab で操作欄（「図」）へ出たとき、図に戻した後は「ソース」へフォーカスを渡す
  // （「図」は図の表示で消えるため、そのままだとフォーカスが文書の外へ落ちる）。
  const focusIntoChrome = React.useRef(false);
  const sourceToggleRef = React.useRef<HTMLButtonElement>(null);
  React.useEffect(() => {
    if (prevInside.current === inside) return;
    prevInside.current = inside;
    setMode(inside ? "source" : "figure");
  }, [inside]);
  React.useEffect(() => {
    if (!focusIntoChrome.current) return;
    focusIntoChrome.current = false;
    if (mode === "figure") sourceToggleRef.current?.focus();
  }, [mode]);

  const showSource = state.kind !== "ok" || (editable && mode === "source");

  const openSource = () => {
    setMode("source");
    const pos = getPos();
    if (typeof pos !== "number") return;
    // 記述の末尾へカーソルを置く（そのまま続きを書ける）。
    editor
      .chain()
      .focus()
      .setTextSelection(pos + 1 + node.content.size)
      .run();
  };
  const showFigure = () => {
    setMode("figure");
    const pos = getPos();
    if (typeof pos !== "number") return;
    // 隠した記述の中にカーソルを残さない（見えない場所へ入力しない）。ブロックの直後へ移す。
    const { state: es, view } = editor;
    const after = pos + node.nodeSize;
    const { from, to } = es.selection;
    if (!(from > pos && to < after)) return;
    const sel = TextSelection.near(es.doc.resolve(Math.min(after, es.doc.content.size)), 1);
    if (sel.from > pos && sel.to < after) return;
    view.dispatch(es.tr.setSelection(sel));
  };

  let status: { text: string; tone: "error" | "muted" } | null = null;
  if (state.kind === "empty") {
    status = { text: "1 行目に種類と題名を書きます（例: bar 月別売上）", tone: "muted" };
  } else if (state.kind === "error") {
    status = {
      text: state.diagnostic ? lineMessage(state.diagnostic) : "図にできませんでした",
      tone: "error",
    };
  } else if (state.kind === "incomplete") {
    status = { text: "データの行を書くと図になります（例: 1月 | 120）", tone: "muted" };
  } else if (showSource) {
    const warning = result.diagnostics.find((d) => d.severity === "warning");
    status = warning ? { text: lineMessage(warning), tone: "muted" } : null;
  }
  if (!editable && state.kind !== "ok") {
    status = {
      text:
        state.kind === "error" && state.diagnostic
          ? `図にできなかったため、記述をそのまま表示しています。${lineMessage(state.diagnostic)}`
          : "図にできなかったため、記述をそのまま表示しています。",
      tone: "muted",
    };
  }

  return (
    <NodeViewWrapper
      className="note-yui"
      data-testid="note-yui-block"
      data-mode={showSource ? "source" : "figure"}
      data-state={state.kind}
    >
      {/* 記述（コード編集そのもの）。ProseMirror の contentDOM なので、図表示中も外さず隠すだけ。 */}
      <pre hidden={!showSource} data-testid="note-yui-source" spellCheck={false}>
        <NodeViewContent<"code"> as="code" className="language-ui" />
      </pre>

      {showSource ? (
        <div
          contentEditable={false}
          data-yui-chrome=""
          onFocus={() => {
            focusIntoChrome.current = true;
          }}
          className="mt-1 flex select-none flex-wrap items-center justify-end gap-x-2 gap-y-1 text-[11px] text-muted-foreground"
        >
          {status ? (
            <span
              // 入力のたびに読み上げを割り込ませない（polite）。
              aria-live="polite"
              className={cn("mr-auto min-w-0", status.tone === "error" && "text-destructive")}
              data-testid={status.tone === "error" ? "note-yui-error" : "note-yui-hint"}
            >
              {status.text}
            </span>
          ) : null}
          {editable ? (
            <ToolbarButton
              onClick={showFigure}
              disabled={state.kind !== "ok"}
              title={state.kind !== "ok" ? "図にできる記述になると切り替えられます" : undefined}
              className="disabled:pointer-events-none disabled:opacity-50 print:hidden"
              data-testid="note-yui-toggle"
            >
              <BarChart3 className="size-3.5" aria-hidden />
              図
            </ToolbarButton>
          ) : null}
        </div>
      ) : null}

      {state.kind === "ok" && result.figure ? (
        <div contentEditable={false} data-yui-chrome="" className="select-none">
          {showSource ? (
            // 編集中は図を下に描き直して見せる（操作欄は図の表示に戻ってから）。
            <div className="mt-2 rounded-lg border border-dashed border-border/80 px-3 pb-1 pt-0.5 sm:px-4">
              <YuiFigure figure={result.figure} className="-mx-3 sm:-mx-4" />
            </div>
          ) : (
            <div
              onDoubleClick={editable ? openSource : undefined}
              data-testid="note-yui-figure"
            >
              <UiFigure
                source={source}
                complete
                kind={kind}
                lang="ui"
                renderTable={renderFigureTable}
                actions={
                  editable ? (
                    <ToolbarButton
                      ref={sourceToggleRef}
                      onClick={openSource}
                      label="図の記述（ソース）を編集"
                      className="print:hidden"
                      data-testid="note-yui-toggle"
                    >
                      <Code2 className="size-3.5" aria-hidden />
                      ソース
                    </ToolbarButton>
                  ) : null
                }
              />
            </div>
          )}
        </div>
      ) : null}
    </NodeViewWrapper>
  );
}

/// StarterKit の codeBlock を置き換える（StarterKit 側は `codeBlock: false`）。
/// 入力規則（```ui + 空白）・キー操作・スキーマ・HTML 往復は CodeBlock のまま。
export const YuiCodeBlock = CodeBlock.extend({
  addNodeView() {
    const yuiView = ReactNodeViewRenderer(YuiCodeBlockView, {
      // 図・操作欄（data-yui-chrome）のイベントは ProseMirror に渡さない（クリックでブロックが
      // ノード選択になって入力で置き換わる・図のキー操作が選択移動に取られる、を防ぐ）。
      stopEvent: ({ event }) =>
        event.target instanceof Element && event.target.closest("[data-yui-chrome]") !== null,
      // 言語が Yui でなくなったら作り直す（既定の <pre><code> 描画へ戻す）。
      update: ({ oldNode, newNode, updateProps }) => {
        if (isYuiNode(oldNode) !== isYuiNode(newNode)) return false;
        updateProps();
        return true;
      },
    });
    // Yui 以外は NodeView を作らない（null で ProseMirror の既定描画＝renderHTML になる）。
    return (props) => (isYuiNode(props.node) ? yuiView(props) : (null as never));
  },
});
