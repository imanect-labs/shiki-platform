"use client";

/// ノートで引用箇所をハイライトする（引用からのディープリンク・#508）。
///
/// ノートは共同編集で内容が変わるので、オフセットではなく一節の本文で探す
/// （[`locatePhrase`]・空白の違いは無視）。見つかった範囲には ProseMirror の Decoration を
/// 付ける。**Yjs のマークにはしない**（他の人にも見え、保存もされてしまうため）。
/// 見つからなければ見出しまで移り、「編集されている」と伝える。
import * as React from "react";
import type { Editor } from "@tiptap/react";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import type { Node as PmNode } from "@tiptap/pm/model";
import { Decoration, DecorationSet } from "@tiptap/pm/view";

import { locatePhrase } from "@/lib/citation-locate";

const citeKey = new PluginKey<DecorationSet>("cite-highlight");

/// ハイライトの見た目（四季の秋色を薄く敷く）。
const HIGHLIGHT_STYLE =
  "background: color-mix(in srgb, var(--season-autumn) 22%, transparent); border-radius: 3px; " +
  "box-decoration-break: clone; -webkit-box-decoration-break: clone;";

function citePlugin(): Plugin<DecorationSet> {
  return new Plugin<DecorationSet>({
    key: citeKey,
    state: {
      init: () => DecorationSet.empty,
      apply(tr, set) {
        const meta = tr.getMeta(citeKey) as { from: number; to: number } | null | undefined;
        if (meta === null) return DecorationSet.empty;
        if (meta) {
          return DecorationSet.create(tr.doc, [
            Decoration.inline(meta.from, meta.to, { class: "cite-highlight", style: HIGHLIGHT_STYLE }),
          ]);
        }
        // 共同編集で本文が変わっても、範囲は変更に追従させる。
        return set.map(tr.mapping, tr.doc);
      },
    },
    props: {
      decorations: (state) => citeKey.getState(state),
    },
  });
}

/// 本文を 1 文字（UTF-16 単位）ずつ並べ、各文字の ProseMirror 位置を添える。段落の境目には
/// 改行を挟む（照合では空白として無視される）。
function flatten(doc: PmNode): { chars: string[]; pos: number[] } {
  const chars: string[] = [];
  const pos: number[] = [];
  doc.descendants((node, at) => {
    if (node.isText && node.text) {
      for (let i = 0; i < node.text.length; i++) {
        chars.push(node.text[i]);
        pos.push(at + i);
      }
    } else if (node.isBlock && chars.length > 0) {
      chars.push("\n");
      pos.push(at);
    }
    return true;
  });
  return { chars, pos };
}

/// 見出しの位置（テキストが一致する最初の見出し）。
function findHeading(doc: PmNode, text: string): number | null {
  const want = text.replace(/\s+/g, "");
  let found: number | null = null;
  doc.descendants((node, at) => {
    if (found != null) return false;
    if (node.type.name === "heading" && node.textContent.replace(/\s+/g, "") === want) {
      found = at + 1;
      return false;
    }
    return true;
  });
  return found;
}

function scrollToPos(editor: Editor, pos: number) {
  try {
    const { node } = editor.view.domAtPos(pos);
    const el = node instanceof HTMLElement ? node : node.parentElement;
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  } catch {
    /* 位置が描画外（折りたたみ等）なら動かさない */
  }
}

export type CiteHighlightResult =
  | { status: "found"; phrase: string }
  | { status: "heading"; heading: string }
  | { status: "missing" };

/// `find`（〜`end`）の一節をハイライトしてスクロールする。同期完了（`synced`）を待ってから 1 回だけ探す。
export function useCiteHighlight(
  editor: Editor | null,
  synced: boolean,
  target: { find: string | null; end: string | null; heading: string | null },
): CiteHighlightResult | null {
  const [result, setResult] = React.useState<CiteHighlightResult | null>(null);
  const { find, end, heading } = target;
  const done = React.useRef(false);

  React.useEffect(() => {
    if (!editor || !find) return;
    editor.registerPlugin(citePlugin());
    return () => {
      if (!editor.isDestroyed) editor.unregisterPlugin(citeKey);
    };
  }, [editor, find]);

  React.useEffect(() => {
    if (!editor || !synced || !find || done.current) return;
    // 同期直後は初回描画が終わっていないことがあるので、1 フレーム待ってから探す。
    const raf = window.requestAnimationFrame(() => {
      if (editor.isDestroyed) return;
      done.current = true;
      const doc = editor.state.doc;
      const { chars, pos } = flatten(doc);
      const hit = locatePhrase(chars, find, end);
      if (hit) {
        const from = pos[hit[0]];
        const to = pos[hit[1] - 1] + 1;
        editor.view.dispatch(editor.state.tr.setMeta(citeKey, { from, to }));
        scrollToPos(editor, from);
        setResult({ status: "found", phrase: find });
        return;
      }
      const at = heading ? findHeading(doc, heading) : null;
      if (at != null && heading) {
        scrollToPos(editor, at);
        setResult({ status: "heading", heading });
        return;
      }
      setResult({ status: "missing" });
    });
    return () => window.cancelAnimationFrame(raf);
  }, [editor, synced, find, end, heading]);

  return result;
}
