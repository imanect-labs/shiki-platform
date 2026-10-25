/// 対話トリガの入力項目（IR の `input_schema`・JSON Schema のサブセット）の読み書き（GAP-12）。
///
/// エンジンが受ける語彙は crates/workflow-engine/src/ir/input_schema.rs（`type`・`properties`・`required`・
/// `enum` などの検証語と、`title`・`description`・`format` などの読み飛ばす注記）。
/// エディタの表で編集できるのは、その中の「1 階層のオブジェクトで、項目ごとに型 1 つ（文字列・数値・整数・
/// 真偽・日付・選択肢）と表示名・説明・必須」の形だけ。範囲・パターン・入れ子などを使った形は壊さず、
/// JSON での編集に回す（`parseInputSchema` が null を返す）。
///
/// 日付は `{ type: "string", format: "date" }`。`format` はエンジンが読み飛ばす注記なので、形の検査はしない
/// （画面の日付の入力欄が `YYYY-MM-DD` を送る）。
/// 項目のキーはミニアプリの画面の部品の `as` の名前と対応する（`件名 = text as title` → `title`）。

export type InputFieldKind = "string" | "number" | "integer" | "boolean" | "date" | "enum";

export const INPUT_FIELD_KINDS: { value: InputFieldKind; label: string }[] = [
  { value: "string", label: "文字列" },
  { value: "number", label: "数値" },
  { value: "integer", label: "整数" },
  { value: "boolean", label: "真偽（はい／いいえ）" },
  { value: "date", label: "日付" },
  { value: "enum", label: "選択肢" },
];

/// 表の 1 行（1 項目）。
export type InputFieldRow = {
  key: string;
  /// 表示名（`title`・空なら書かない）。
  title: string;
  kind: InputFieldKind;
  required: boolean;
  /// 選択肢（`enum` のときだけ書く）。
  options: string[];
  /// 説明（`description`・表では編集しないがそのまま書き戻す）。
  description?: string;
};

export type InputSchemaForm = {
  fields: InputFieldRow[];
  /// 表では編集しない最上位の語（`additionalProperties`・注記）。そのまま書き戻す。
  rest: Record<string, unknown>;
};

/// キーの形（英字か _ で始まる英数字と _）。数字だけのキーは JSON のオブジェクトで並びが変わるので避ける。
export const INPUT_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

const TOP_KEYS = new Set(["type", "properties", "required", "additionalProperties", "title", "description", "$schema", "$comment"]);
const REST_KEYS = ["additionalProperties", "title", "description", "$schema", "$comment"];
const FIELD_KEYS = new Set(["type", "title", "description", "format", "enum"]);

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function optionalText(v: unknown): string | null | undefined {
  if (v === undefined) return undefined;
  return typeof v === "string" ? v : null;
}

function parseField(key: string, spec: unknown, required: boolean): InputFieldRow | null {
  if (!isRecord(spec) || Object.keys(spec).some((k) => !FIELD_KEYS.has(k))) return null;
  const title = optionalText(spec.title);
  const description = optionalText(spec.description);
  if (title === null || description === null) return null;
  const base = { key, title: title ?? "", required, options: [] as string[], ...(description !== undefined ? { description } : {}) };
  const { type, format } = spec;
  if (spec.enum !== undefined) {
    const opts = spec.enum;
    if (type !== "string" || format !== undefined) return null;
    if (!Array.isArray(opts) || !opts.every((o) => typeof o === "string")) return null;
    return { ...base, kind: "enum", options: opts as string[] };
  }
  if (format !== undefined) {
    return type === "string" && format === "date" ? { ...base, kind: "date" } : null;
  }
  if (type === "string" || type === "number" || type === "integer" || type === "boolean") {
    return { ...base, kind: type };
  }
  return null;
}

/// 表で編集できる形なら読み、できなければ null（JSON で編集する）。未定義（null）は項目なし。
export function parseInputSchema(schema: unknown): InputSchemaForm | null {
  if (schema === null || schema === undefined) return { fields: [], rest: {} };
  if (!isRecord(schema) || Object.keys(schema).some((k) => !TOP_KEYS.has(k))) return null;
  if (schema.type !== undefined && schema.type !== "object") return null;
  if (schema.additionalProperties !== undefined && typeof schema.additionalProperties !== "boolean") return null;
  const props = schema.properties ?? {};
  if (!isRecord(props)) return null;
  const required = schema.required ?? [];
  if (!Array.isArray(required) || !required.every((r) => typeof r === "string" && r in props)) return null;
  const fields: InputFieldRow[] = [];
  for (const [key, spec] of Object.entries(props)) {
    // 表で書けないキー（日本語のラベルをそのままキーにしたもの等）は、表で直すと誤りになるので JSON で扱う。
    if (!INPUT_KEY_RE.test(key)) return null;
    const row = parseField(key, spec, required.includes(key));
    if (!row) return null;
    fields.push(row);
  }
  const rest: Record<string, unknown> = {};
  for (const k of REST_KEYS) if (schema[k] !== undefined) rest[k] = schema[k];
  return { fields, rest };
}

function fieldSchema(f: InputFieldRow): Record<string, unknown> {
  const out: Record<string, unknown> =
    f.kind === "date"
      ? { type: "string", format: "date" }
      : f.kind === "enum"
        ? { type: "string", enum: f.options }
        : { type: f.kind };
  if (f.title.trim() !== "") out.title = f.title.trim();
  if (f.description !== undefined) out.description = f.description;
  return out;
}

/// 表の内容を input_schema に書き戻す。項目も最上位の語も無ければ null（入力の定義なし）。
export function buildInputSchema(form: InputSchemaForm): unknown {
  if (form.fields.length === 0 && Object.keys(form.rest).length === 0) return null;
  const properties: Record<string, unknown> = {};
  for (const f of form.fields) properties[f.key] = fieldSchema(f);
  const required = form.fields.filter((f) => f.required).map((f) => f.key);
  return { type: "object", properties, ...(required.length > 0 ? { required } : {}), ...form.rest };
}

/// 行ごとの誤り（行の位置 → 文言）。誤りがある間は IR に書き戻さない。
export function inputRowErrors(fields: InputFieldRow[]): Map<number, string> {
  const errors = new Map<number, string>();
  const seen = new Map<string, number>();
  fields.forEach((f, i) => {
    if (f.key === "") errors.set(i, "キーを入れてください");
    else if (!INPUT_KEY_RE.test(f.key)) errors.set(i, "キーは英字か _ で始め、英数字と _ だけで書きます");
    else if (seen.has(f.key)) errors.set(i, `キー「${f.key}」が重なっています`);
    else if (f.kind === "enum" && f.options.length === 0) errors.set(i, "選択肢を 1 つ以上入れてください");
    else if (f.kind === "enum" && new Set(f.options).size !== f.options.length) errors.set(i, "同じ選択肢が重なっています");
    if (!seen.has(f.key)) seen.set(f.key, i);
  });
  return errors;
}

/// 差し込みの候補・実行の画面に使う、項目の緩い読み（表で編集できない形でも properties があれば読む）。
export type InputFieldInfo = {
  key: string;
  title: string | null;
  kind: InputFieldKind | "other";
  options: string[];
  required: boolean;
  description: string | null;
};

export function inputFieldsOf(schema: unknown): InputFieldInfo[] {
  if (!isRecord(schema) || !isRecord(schema.properties)) return [];
  const required = Array.isArray(schema.required) ? schema.required : [];
  return Object.entries(schema.properties).map(([key, spec]) => {
    const s = isRecord(spec) ? spec : {};
    const enumOpts = Array.isArray(s.enum) ? s.enum.filter((o): o is string => typeof o === "string") : [];
    const kind: InputFieldKind | "other" =
      Array.isArray(s.enum) && enumOpts.length === s.enum.length && enumOpts.length > 0
        ? "enum"
        : s.type === "string" && s.format === "date"
          ? "date"
          : s.type === "string" || s.type === "number" || s.type === "integer" || s.type === "boolean"
            ? s.type
            : "other";
    return {
      key,
      title: typeof s.title === "string" && s.title !== "" ? s.title : null,
      kind,
      options: enumOpts,
      required: required.includes(key),
      description: typeof s.description === "string" && s.description !== "" ? s.description : null,
    };
  });
}
