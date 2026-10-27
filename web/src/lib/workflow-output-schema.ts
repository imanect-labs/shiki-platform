/// 「AI に聞く」（llm.invoke）の出力の形（`output_schema`・JSON Schema のサブセット）の読み書き（GAP-13）。
///
/// 保存時の検査の正は crates/workflow-engine/src/ir/output_schema.rs（`check_output_schema`）で、ここはエディタの
/// 表で組める形を読み書きするだけ（上限・キーワードの検査はサーバに任せ、誤りは欄に写す）。
/// 表で編集できるのは「1 階層のオブジェクトで、項目ごとに型 1 つ（文字列・数値・整数・はい/いいえ・選択肢・
/// 文字の一覧）と説明・必須」の形だけ。入れ子・null を許す型・範囲などを使った形は壊さず JSON での編集に回す
/// （`parseOutputSchema` が null を返す）。
///
/// - オブジェクトは閉じる（`additionalProperties` は書かない＝閉じたものとして扱う・ir.md §7.3）。
/// - 項目の並びは注記の `x-order`（D-60）が持つ（IR は jsonb の列に保存され、`properties` のキーの順は保存で
///   変わる）。表は書き戻すたびに今の並びを `x-order` に書き、読む側は `orderedProperties` で並べる。
///   gateway はプロバイダへ送る前に `x-order` を落とす。
/// - 説明（`description`）はモデルへの手がかりとしてそのまま送られる。

import { INPUT_KEY_RE, INPUT_ORDER_KEY, orderedProperties } from "@/lib/workflow-input-schema";

export type OutputFieldKind = "string" | "number" | "integer" | "boolean" | "enum" | "string_list";

export const OUTPUT_FIELD_KINDS: { value: OutputFieldKind; label: string }[] = [
  { value: "string", label: "文字列" },
  { value: "number", label: "数値" },
  { value: "integer", label: "整数" },
  { value: "boolean", label: "はい／いいえ" },
  { value: "enum", label: "選択肢" },
  { value: "string_list", label: "文字の一覧" },
];

/// 表の 1 行（出力の 1 項目）。
export type OutputFieldRow = {
  key: string;
  kind: OutputFieldKind;
  required: boolean;
  /// 選択肢（`enum` のときだけ書く）。
  options: string[];
  /// 説明（`description`・空なら書かない）。モデルへの手がかりになる。
  description: string;
  /// 表示名（`title`・表では編集しないがそのまま書き戻す）。
  title?: string;
};

export type OutputSchemaForm = {
  fields: OutputFieldRow[];
  /// 表では編集しない最上位の語（注記）。そのまま書き戻す。
  rest: Record<string, unknown>;
};

/// 項目名の形（英字か _ で始まる英数字と _・64 字まで。エンジンの `valid_property_name` と同じ）。
export const OUTPUT_KEY_RE = INPUT_KEY_RE;

const TOP_KEYS = new Set([
  "type",
  "properties",
  "required",
  "additionalProperties",
  "title",
  "description",
  "$comment",
  INPUT_ORDER_KEY,
]);
const REST_KEYS = ["title", "description", "$comment"];
const FIELD_KEYS = new Set(["type", "title", "description", "enum", "items"]);

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function optionalText(v: unknown): string | null | undefined {
  if (v === undefined) return undefined;
  return typeof v === "string" ? v : null;
}

function parseField(key: string, spec: unknown, required: boolean): OutputFieldRow | null {
  if (!isRecord(spec) || Object.keys(spec).some((k) => !FIELD_KEYS.has(k))) return null;
  const title = optionalText(spec.title);
  const description = optionalText(spec.description);
  if (title === null || description === null) return null;
  const base = {
    key,
    required,
    options: [] as string[],
    description: description ?? "",
    ...(title !== undefined ? { title } : {}),
  };
  const { type, items } = spec;
  if (spec.enum !== undefined) {
    const opts = spec.enum;
    if (type !== "string" || items !== undefined) return null;
    if (!Array.isArray(opts) || opts.length === 0 || !opts.every((o) => typeof o === "string")) return null;
    return { ...base, kind: "enum", options: opts as string[] };
  }
  if (type === "array") {
    // 文字の一覧（`items: { type: "string" }`）だけを表で扱う。
    if (!isRecord(items) || Object.keys(items).length !== 1 || items.type !== "string") return null;
    return { ...base, kind: "string_list" };
  }
  if (items !== undefined) return null;
  if (type === "string" || type === "number" || type === "integer" || type === "boolean") {
    return { ...base, kind: type };
  }
  return null;
}

/// 表で編集できる形なら読み、できなければ null（JSON で編集する）。未定義（undefined / null）は項目なし。
export function parseOutputSchema(schema: unknown): OutputSchemaForm | null {
  if (schema === null || schema === undefined) return { fields: [], rest: {} };
  if (!isRecord(schema) || Object.keys(schema).some((k) => !TOP_KEYS.has(k))) return null;
  if (schema.type !== "object") return null;
  // 開いたオブジェクトは保存できない（サーバが止める）。表で書き直して黙って閉じない。
  if (schema.additionalProperties !== undefined && schema.additionalProperties !== false) return null;
  const props = schema.properties ?? {};
  if (!isRecord(props)) return null;
  const required = schema.required ?? [];
  if (
    !Array.isArray(required) ||
    !required.every((r) => typeof r === "string" && Object.hasOwn(props, r)) ||
    new Set(required).size !== required.length
  ) {
    return null;
  }
  const order = schema[INPUT_ORDER_KEY];
  if (
    order !== undefined &&
    !(
      Array.isArray(order) &&
      order.every((k) => typeof k === "string" && Object.hasOwn(props, k)) &&
      new Set(order).size === order.length
    )
  ) {
    return null;
  }
  const fields: OutputFieldRow[] = [];
  for (const [key, spec] of orderedProperties(schema)) {
    if (!OUTPUT_KEY_RE.test(key)) return null;
    const row = parseField(key, spec, required.includes(key));
    if (!row) return null;
    fields.push(row);
  }
  const rest: Record<string, unknown> = {};
  for (const k of REST_KEYS) if (schema[k] !== undefined) rest[k] = schema[k];
  return { fields, rest };
}

function fieldSchema(f: OutputFieldRow): Record<string, unknown> {
  const out: Record<string, unknown> =
    f.kind === "enum"
      ? { type: "string", enum: f.options }
      : f.kind === "string_list"
        ? { type: "array", items: { type: "string" } }
        : { type: f.kind };
  if (f.title !== undefined) out.title = f.title;
  // 入力中の値をそのまま書く（trim すると、語の間に打った空白が次の描画で消えて打てない）。前後の空白は欄を
  // 離れたときに落とす（output-schema-editor.tsx）。空かどうかだけを trim で見る。
  if (f.description.trim() !== "") out.description = f.description;
  return out;
}

/// 表の内容を output_schema に書き戻す。項目が無ければ undefined（出力の形を決めない＝文章だけを受け取る）。
export function buildOutputSchema(form: OutputSchemaForm): Record<string, unknown> | undefined {
  if (form.fields.length === 0) return undefined;
  const properties: Record<string, unknown> = {};
  for (const f of form.fields) properties[f.key] = fieldSchema(f);
  const required = form.fields.filter((f) => f.required).map((f) => f.key);
  return {
    type: "object",
    properties,
    ...(required.length > 0 ? { required } : {}),
    [INPUT_ORDER_KEY]: form.fields.map((f) => f.key),
    ...form.rest,
  };
}

/// 行ごとの誤り（行の位置 → 文言）。誤りがある間は IR に書き戻さない。
export function outputRowErrors(fields: OutputFieldRow[]): Map<number, string> {
  const errors = new Map<number, string>();
  const seen = new Set<string>();
  fields.forEach((f, i) => {
    if (f.key === "") errors.set(i, "項目名を入れてください");
    else if (!OUTPUT_KEY_RE.test(f.key)) errors.set(i, "項目名は英字か _ で始め、英数字と _ だけで書きます（64 字まで）");
    else if (seen.has(f.key)) errors.set(i, `項目名「${f.key}」が重なっています`);
    else if (f.kind === "enum" && f.options.length === 0) errors.set(i, "選択肢を 1 つ以上入れてください");
    else if (f.kind === "enum" && new Set(f.options).size !== f.options.length) errors.set(i, "同じ選択肢が重なっています");
    seen.add(f.key);
  });
  return errors;
}

/// 出力の形の最上位の項目名（`x-order` の順）。差し込みの候補（`/data/<項目>`）に使う。形が読めなければ空。
export function outputSchemaKeys(schema: unknown): string[] {
  return orderedProperties(schema)
    .map(([k]) => k)
    .filter((k) => OUTPUT_KEY_RE.test(k));
}
