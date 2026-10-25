/// ワークフローの対話トリガの `input_schema`（JSON Schema のサブセット・crates/workflow-engine の
/// ir/input_schema.rs）から、画面の組み立てで使う入力の手がかり（`WorkflowInputHint`）を作る。
///
/// 手がかりは部品の候補を出し、足りない必須の入力を知らせるためだけに使う（便宜）。値の検証は
/// 起動のときにサーバーが同じスキーマで行うので、ここで読めない形は黙って飛ばしてよい。

import type { WorkflowInputHint } from "@imanect/yui/editor";

type Prop = {
  type?: string | string[];
  title?: string;
  format?: string;
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
  maxLength?: number;
  items?: { enum?: unknown[] };
};

export function inputHints(schema: unknown): WorkflowInputHint[] {
  if (!schema || typeof schema !== "object") return [];
  const s = schema as { properties?: Record<string, Prop>; required?: unknown };
  if (!s.properties || typeof s.properties !== "object") return [];
  const required = new Set(Array.isArray(s.required) ? s.required.filter((x): x is string => typeof x === "string") : []);
  return Object.entries(s.properties).flatMap(([key, p]): WorkflowInputHint[] => {
    if (!p || typeof p !== "object") return [];
    const type = Array.isArray(p.type) ? p.type.find((t) => t !== "null") : p.type;
    const hint: WorkflowInputHint = { key, required: required.has(key) };
    if (typeof p.title === "string" && p.title.trim()) hint.label = p.title.trim().slice(0, 40);
    const options = (p.enum ?? p.items?.enum)?.filter((v): v is string => typeof v === "string");
    if (options && options.length >= 2) {
      hint.type = "choice";
      hint.options = options.slice(0, 20);
      // 値の配列（items.enum）は複数選択（文字列 1 つを送ると、起動のときに入力の検証で断られる）
      if (type === "array") hint.multi = true;
    } else if (type === "number" || type === "integer") {
      hint.type = "number";
      if (typeof p.minimum === "number" && typeof p.maximum === "number") Object.assign(hint, { min: p.minimum, max: p.maximum });
    } else if (type === "boolean") hint.type = "toggle";
    else if (type === "string" && p.format === "date") hint.type = "date";
    else if (type === "string") hint.type = typeof p.maxLength === "number" && p.maxLength > 200 ? "textarea" : "text";
    else return [];
    return [hint];
  });
}
