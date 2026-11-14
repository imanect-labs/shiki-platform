/// 画面の入力と、束ねたワークフローの入力項目の突き合わせ（GAP-12）。
///
/// - 入力の手がかり（部品の候補と、必須の入力が足りるかの手がかり）は、サーバーが入力項目から作って返す
///   （`POST /mini-apps/workflow-inputs`・決まりはサーバーにだけ置く）。ここでは Yui のエディタの型に合わせるだけ。
/// - 「書く」タブの突き合わせは、保存のときと同じ関数をサーバーが掛ける（`POST /mini-apps/input-check`・読む図も同じ
///   エラーの無い図・BUG-221）。組み立てるは Yui のエディタが自分で突き合わせる。読めないワークフローは突き合わせない
///   （BUG-122）。

import type { WorkflowInputHint } from "@imanect/yui/editor";

import type { components } from "@/generated/api";

type ServerHint = components["schemas"]["WorkflowInputHint"];

/// サーバーの手がかりを Yui のエディタの型へ（値の無い項目は付けない）。
export function toYuiHints(hints: readonly ServerHint[]): WorkflowInputHint[] {
  return hints.map((h) => {
    const out: WorkflowInputHint = { key: h.key, type: h.type, required: h.required };
    if (h.label != null) out.label = h.label;
    if (h.options != null) out.options = h.options;
    if (h.multi != null) out.multi = h.multi;
    if (h.min != null && h.max != null) Object.assign(out, { min: h.min, max: h.max });
    return out;
  });
}
