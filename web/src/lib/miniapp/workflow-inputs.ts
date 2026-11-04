/// 画面の入力と、束ねたワークフローの入力項目の突き合わせ（GAP-12）。
///
/// - 入力の手がかり（部品の候補と、必須の入力が足りるかの手がかり）は、サーバーが入力項目から作って返す
///   （`POST /mini-apps/workflow-inputs`・決まりはサーバーにだけ置く）。ここでは Yui のエディタの型に合わせるだけ。
/// - 「書く」タブでも、組み立てると同じ突き合わせ（Yui の `checkApp`）を、読めた図に掛けて知らせる。保存のときには
///   サーバーが同じ規則で確かめ直す（`input_warnings`）。読めないワークフローは突き合わせない（BUG-122）。

import { checkApp, type WorkflowChoice, type WorkflowInputHint } from "@imanect/yui/editor";

import type { components } from "@/generated/api";
import { splitScreen } from "@/lib/miniapp/screen-blocks";

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

/// 入力の突き合わせの注意のコード（Yui のエディタ・サーバーの `miniapp.<code>` と同じ）。
const INPUT_CODES = new Set(["missing_input", "input_type", "input_required", "unknown_input"]);

/// 「書く」タブの記述の、入力の突き合わせの注意（図と部品の場所を前に付ける・サーバーの注意と同じ形）。
///
/// 読めない図（書き損じ・閉じていない）は突き合わせない（ボタンが出ないので送られない）。
export function inputNotes(screen: string, workflows: readonly WorkflowChoice[]): string[] {
  if (!workflows.some((w) => w.inputs)) return [];
  const out: string[] = [];
  for (const block of splitScreen(screen)) {
    if (block.type !== "app") continue;
    const f = block.figure;
    for (const p of checkApp(f, { workflows, allowChat: false })) {
      if (!INPUT_CODES.has(p.code)) continue;
      const section = p.path.section != null ? f.sections[p.path.section] : undefined;
      const field = p.path.field != null ? section?.fields[p.path.field] : undefined;
      const action = p.path.action != null ? section?.actions[p.path.action] : undefined;
      const part = field ? `の「${field.label}」` : action ? `のボタン「${action.label}」` : "";
      out.push(`画面の図「${f.title}」${part}: ${p.message}`);
    }
  }
  return out;
}
