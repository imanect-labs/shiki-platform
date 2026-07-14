/// ライブプレビュー: フォーカス行の Markdown 記法を可視化する（Task 11P.3 / issue #297）。
///
/// Obsidian の live preview 相当。カーソルのあるブロックに限り、隠れている記法
/// （見出しの `#`、強調 `**`/`*`、打ち消し `~~`、インラインコード `` ` ``、引用 `>`、
/// リンク `[...](url)`）を淡色の**ウィジェット装飾**として重ねる。フォーカスが外れた
/// ブロックは従来どおり整形表示に戻る。
///
/// 共同編集セーフ: ウィジェットは Decoration（表示専用）でありドキュメントを一切変更
/// しない。Yjs には何も書き込まない（フォーカス状態はプラグイン内部状態＝meta のみ）。

import { Extension } from "@tiptap/core";
import type { Node as PMNode, Mark } from "@tiptap/pm/model";
import { Plugin, PluginKey, type EditorState } from "@tiptap/pm/state";
import { Decoration, DecorationSet, type EditorView } from "@tiptap/pm/view";

/// 記法の見えないインラインマーク → 表示する区切り文字。
const INLINE_DELIMS: Record<string, string> = {
  bold: "**",
  italic: "*",
  strike: "~~",
  code: "`",
};

const pluginKey = new PluginKey<{ focused: boolean }>("notesLivePreview");

interface Run {
  from: number;
  to: number;
  mark: Mark;
}

export const LivePreview = Extension.create({
  name: "notesLivePreview",

  addProseMirrorPlugins() {
    return [
      new Plugin<{ focused: boolean }>({
        key: pluginKey,
        state: {
          init: () => ({ focused: false }),
          apply(tr, value) {
            const meta = tr.getMeta(pluginKey) as { focused: boolean } | undefined;
            return meta ?? value;
          },
        },
        props: {
          // DOM フォーカスをプラグイン状態へ反映（装飾の on/off に使う・doc は不変）。
          handleDOMEvents: {
            focus: (view) => {
              setFocused(view, true);
              return false;
            },
            blur: (view) => {
              setFocused(view, false);
              return false;
            },
          },
          decorations(state) {
            const focused = pluginKey.getState(state)?.focused ?? false;
            if (!focused) return DecorationSet.empty;
            return buildDecorations(state);
          },
        },
      }),
    ];
  },
});

function setFocused(view: EditorView, focused: boolean): void {
  const current = pluginKey.getState(view.state)?.focused ?? false;
  if (current === focused) return;
  view.dispatch(view.state.tr.setMeta(pluginKey, { focused }));
}

/// 選択が触れているブロックの記法をウィジェット装飾で可視化する。
function buildDecorations(state: EditorState): DecorationSet {
  const { selection, doc } = state;
  const decorations: Decoration[] = [];

  doc.nodesBetween(selection.from, selection.to, (node, pos, parent) => {
    if (!node.isTextblock) return true; // コンテナは降りて中の textblock を探す。

    // ブロック先頭マーカー（見出しの #、引用の >）。
    const contentStart = pos + 1;
    if (node.type.name === "heading") {
      const level = clampLevel(node.attrs.level);
      decorations.push(marker(contentStart, `${"#".repeat(level)} `, -1, `h:${pos}`));
    }
    if (parent?.type.name === "blockquote") {
      decorations.push(marker(contentStart, "> ", -1, `q:${pos}`));
    }

    // インラインマークの区切り可視化。
    for (const markName of Object.keys(INLINE_DELIMS)) {
      const delim = INLINE_DELIMS[markName];
      for (const run of markRuns(node, pos, markName)) {
        decorations.push(marker(run.from, delim, -1, `${markName}:${run.from}:o`));
        decorations.push(marker(run.to, delim, 1, `${markName}:${run.to}:c`));
      }
    }
    // リンクは [text](href) を復元する。
    for (const run of markRuns(node, pos, "link")) {
      const href = typeof run.mark.attrs.href === "string" ? run.mark.attrs.href : "";
      decorations.push(marker(run.from, "[", -1, `link:${run.from}:o`));
      decorations.push(marker(run.to, `](${href})`, 1, `link:${run.to}:c`));
    }

    return false; // textblock 内のインラインは自前で処理済み（降りない）。
  });

  return DecorationSet.create(doc, decorations);
}

/// テキストブロック内の、指定マークが連続する範囲（同一属性で結合）を返す。
function markRuns(block: PMNode, blockPos: number, markName: string): Run[] {
  const runs: Run[] = [];
  let offset = 0;
  let current: Run | null = null;
  block.forEach((child) => {
    const from = blockPos + 1 + offset;
    const size = child.nodeSize;
    const mark = child.isText
      ? child.marks.find((m) => m.type.name === markName)
      : undefined;
    if (mark) {
      if (current && current.to === from && current.mark.eq(mark)) {
        current.to = from + size;
      } else {
        current = { from, to: from + size, mark };
        runs.push(current);
      }
    } else {
      current = null;
    }
    offset += size;
  });
  return runs;
}

/// 淡色の記法ウィジェットを作る（表示専用・選択やコピーに含めない）。
function marker(pos: number, text: string, side: number, key: string): Decoration {
  return Decoration.widget(
    pos,
    () => {
      const el = document.createElement("span");
      el.className = "note-md-marker";
      el.textContent = text;
      el.setAttribute("aria-hidden", "true");
      el.contentEditable = "false";
      return el;
    },
    { side, key, ignoreSelection: true, marks: [] },
  );
}

function clampLevel(level: unknown): number {
  const n = typeof level === "number" ? level : 1;
  return Math.min(6, Math.max(1, n));
}
