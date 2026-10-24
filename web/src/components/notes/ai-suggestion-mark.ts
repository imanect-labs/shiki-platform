/// AI 提案マーク（Task 11P.4・#886）。document.edit の suggest モードが付ける 2 種の印を
/// TipTap のマークとして扱い、視覚的に区別する。
///
/// - `aiSuggestion`: AI が入れようとしているテキスト。
/// - `aiDeletion`: AI が消そうとしているテキスト（消さずに印だけ付けてある）。
///
/// 文字を持たないブロック（区切り線・埋め込み・空の段落）には書式で印を付けられないので、
/// サーバはノードの属性（同じ名前）に付ける。承認/棄却はどちらの形も同じように扱う。
///
/// どちらも **md に落とさない**（Task 11P.2 の往復対象外・Yjs snapshot 側の正本に保つ）。
/// 承認 = 入れる側の印を外して本文化し、消す側を削除。棄却 = 入れる側を削除し、消す側の印を外して元に戻す。

import { Mark, mergeAttributes } from "@tiptap/core";
import type { MarkType, Node as PmNode } from "@tiptap/pm/model";
import type { Transaction } from "@tiptap/pm/state";

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    aiSuggestion: {
      /// 文書全体の提案を承認する（入れる側は本文として残し、消す側は消す）。
      acceptAllSuggestions: () => ReturnType;
      /// 文書全体の提案を棄却する（入れる側は消し、消す側は元に戻す）。
      rejectAllSuggestions: () => ReturnType;
    };
  }
}

/// 表のセルは丸ごと消すと表の形が崩れるので、中身の文字だけを消す。
const KEEP_WHOLE = new Set(["tableCell", "tableHeader"]);

/// ノードの属性として印を持てるブロック（文字を持たないことがあるもの）。
const NODE_MARKABLE = [
  "paragraph",
  "heading",
  "codeBlock",
  "horizontalRule",
  "shikiEmbed",
  "blockquote",
  "bulletList",
  "orderedList",
  "taskList",
  "listItem",
  "taskItem",
  "table",
  "tableRow",
  "tableCell",
  "tableHeader",
];

/// 表の構造は、ブロックの属性に印があるときだけ丸ごと消す（セルの文字だけへの提案で行や表を
/// 消さない・サーバの md 読み出しと同じ）。
const STRUCTURE_ONLY = new Set(["table", "tableRow", "tableCell", "tableHeader"]);

/// ブロックが丸ごと `markType` の印付きか（ノードの属性、または中身がすべて印付き）。
function fullyMarked(node: PmNode, markType: MarkType): boolean {
  if (node.attrs[markType.name] === true) return true;
  if (STRUCTURE_ONLY.has(node.type.name)) return false;
  if (node.isText) return node.marks.some((m) => m.type === markType);
  if (node.isTextblock) {
    // 文字がすべて印付き（強制改行などの文字でないものは問わない）。空の段落は印が無い限り対象外。
    let any = false;
    let all = true;
    node.forEach((child) => {
      if (!child.isText) return;
      any = true;
      if (!child.marks.some((m) => m.type === markType)) all = false;
    });
    return any && all;
  }
  if (node.childCount === 0) return false;
  let all = true;
  node.forEach((child) => {
    if (!fullyMarked(child, markType)) all = false;
  });
  return all;
}

/// `markType` の印が付いた範囲を消す。中身がすべて印付きのブロック（段落・項目・表など）は
/// 空の入れ物を残さないようブロックごと消す。何か消したら true。
function deleteMarked(tr: Transaction, doc: PmNode, markType: MarkType): boolean {
  const ranges: Array<{ from: number; to: number }> = [];
  doc.descendants((node, pos) => {
    if (node.isText) {
      if (node.marks.some((m) => m.type === markType)) ranges.push({ from: pos, to: pos + node.nodeSize });
      return false;
    }
    if (!KEEP_WHOLE.has(node.type.name) && fullyMarked(node, markType)) {
      ranges.push({ from: pos, to: pos + node.nodeSize });
      return false;
    }
    return true;
  });
  // 後ろから削除して位置ずれを避ける。
  for (const r of ranges.reverse()) tr.delete(r.from, r.to);
  return ranges.length > 0;
}

/// `markType` の印だけを外す（文字・ブロックは残す）。何か外したら true。
function unmark(tr: Transaction, doc: PmNode, markType: MarkType): boolean {
  let changed = false;
  doc.descendants((node, pos) => {
    if (node.isText && node.marks.some((m) => m.type === markType)) {
      tr.removeMark(pos, pos + node.nodeSize, markType);
      changed = true;
    } else if (node.attrs[markType.name] === true) {
      tr.setNodeMarkup(pos, undefined, { ...node.attrs, [markType.name]: null });
      changed = true;
    }
  });
  return changed;
}

/// 文書に提案（入れる・消す）が残っている数（文字のかたまり・ブロック）。
export function countSuggestions(doc: PmNode, markTypes: MarkType[]): number {
  let n = 0;
  doc.descendants((node) => {
    if (markTypes.some((t) => node.attrs[t.name] === true)) n += 1;
    else if (node.isText && node.marks.some((m) => markTypes.includes(m.type))) n += 1;
  });
  return n;
}

/// 提案の印をブロックの属性として持たせる（Yjs の要素の属性 ↔ ノードの属性）。
function blockFlag(name: string, dataAttr: string) {
  return {
    default: null,
    keepOnSplit: false,
    parseHTML: (el: HTMLElement) => (el.hasAttribute(dataAttr) ? true : null),
    renderHTML: (attrs: Record<string, unknown>) => (attrs[name] === true ? { [dataAttr]: "true" } : {}),
  };
}

export const AiSuggestionMark = Mark.create({
  name: "aiSuggestion",
  // y-prosemirror が Yjs の formatting attribute `aiSuggestion` をこのマークに対応づける。
  // inclusive=false: 提案範囲の直後の入力は提案扱いにしない。
  inclusive: false,

  addGlobalAttributes() {
    return [
      {
        types: NODE_MARKABLE,
        attributes: {
          aiSuggestion: blockFlag("aiSuggestion", "data-ai-suggestion"),
          aiDeletion: blockFlag("aiDeletion", "data-ai-deletion"),
        },
      },
    ];
  },

  parseHTML() {
    return [{ tag: "span[data-ai-suggestion]" }];
  },

  renderHTML({ HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(HTMLAttributes, {
        "data-ai-suggestion": "true",
        class: "note-ai-suggestion",
      }),
      0,
    ];
  },

  addCommands() {
    return {
      acceptAllSuggestions:
        () =>
        ({ tr, state, dispatch }) => {
          const add = state.schema.marks.aiSuggestion;
          const del = state.schema.marks.aiDeletion;
          if (!add) return false;
          // 先に印を外してから消す（外すのは位置を変えない）。
          const unmarked = unmark(tr, state.doc, add);
          const deleted = del ? deleteMarked(tr, tr.doc, del) : false;
          const changed = unmarked || deleted;
          if (changed && dispatch) dispatch(tr);
          return changed;
        },
      rejectAllSuggestions:
        () =>
        ({ tr, state, dispatch }) => {
          const add = state.schema.marks.aiSuggestion;
          const del = state.schema.marks.aiDeletion;
          if (!add) return false;
          const unmarked = del ? unmark(tr, state.doc, del) : false;
          const deleted = deleteMarked(tr, tr.doc, add);
          const changed = unmarked || deleted;
          if (changed && dispatch) dispatch(tr);
          return changed;
        },
    };
  },
});

/// AI が消そうとしているテキストの印（承認で消え、棄却で元に戻る）。
export const AiDeletionMark = Mark.create({
  name: "aiDeletion",
  inclusive: false,

  parseHTML() {
    return [{ tag: "span[data-ai-deletion]" }];
  },

  renderHTML({ HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(HTMLAttributes, {
        "data-ai-deletion": "true",
        class: "note-ai-deletion",
      }),
      0,
    ];
  },
});
