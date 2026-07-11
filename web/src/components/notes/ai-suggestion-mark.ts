/// AI 提案マーク（Task 11P.4）。document.edit の suggest モードが挿入したテキストに付く
/// `aiSuggestion` フォーマット属性を TipTap のマークとして扱い、視覚的に区別する。
///
/// このマークは **md に落とさない**（Task 11P.2 の往復対象外・Yjs snapshot 側の正本に保つ）。
/// 承認 = マークを外して本文化、棄却 = マークの付いた範囲を削除。

import { Mark, mergeAttributes } from "@tiptap/core";

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    aiSuggestion: {
      /// 文書全体の提案マークを承認する（マークだけ外して本文として残す）。
      acceptAllSuggestions: () => ReturnType;
      /// 文書全体の提案（マークの付いたテキスト）を棄却する（範囲を削除）。
      rejectAllSuggestions: () => ReturnType;
    };
  }
}

export const AiSuggestionMark = Mark.create({
  name: "aiSuggestion",
  // y-prosemirror が Yjs の formatting attribute `aiSuggestion` をこのマークに対応づける。
  // inclusive=false: 提案範囲の直後の入力は提案扱いにしない。
  inclusive: false,

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
          const markType = state.schema.marks.aiSuggestion;
          if (!markType) return false;
          let changed = false;
          state.doc.descendants((node, pos) => {
            if (node.isText && node.marks.some((m) => m.type === markType)) {
              tr.removeMark(pos, pos + node.nodeSize, markType);
              changed = true;
            }
          });
          if (changed && dispatch) dispatch(tr);
          return changed;
        },
      rejectAllSuggestions:
        () =>
        ({ tr, state, dispatch }) => {
          const markType = state.schema.marks.aiSuggestion;
          if (!markType) return false;
          // 後ろから削除して位置ずれを避ける。
          const ranges: Array<{ from: number; to: number }> = [];
          state.doc.descendants((node, pos) => {
            if (node.isText && node.marks.some((m) => m.type === markType)) {
              ranges.push({ from: pos, to: pos + node.nodeSize });
            }
          });
          if (ranges.length === 0) return false;
          for (const r of ranges.reverse()) tr.delete(r.from, r.to);
          if (dispatch) dispatch(tr);
          return true;
        },
    };
  },
});
