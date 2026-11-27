/// 値の欄の「項目を並べる」（項目のまとまり）と「リストを並べる」（リスト）の組み立て（描画を持たない・BUG-118）。
///
/// IR はパラメータ値の任意の位置に `$from` / `$template` を置ける（ir.md §3.1・エンジンは resolve_deep で入れ子まで
/// 解く）。画面は、項目のまとまりとリストを行に分け、行ごとに値の入れ方を選ばせる。行と IR の値は 1 対 1 で、
/// 行から作り直した値は、開いた値と同じになる（ほかの項目と参照を変えずに残す）。
///
/// - 組み立てられるのは入れ子の 2 段まで（欄そのものと、その中の 1 段）。それより深いところに参照や差し込みが
///   あるとき、形の崩れた値式（`$from` のキーがあるが読めない）があるとき、キーが空の項目があるときは、
///   組み立てずに JSON の欄で出す（[`composeBlock`] がその理由を返す）。
/// - 参照も差し込みも無い入れ子（固定のリストなど）は、深くても 1 つの値として JSON で直接入力できる。
/// - 値式の形の判定は、エンジンの読み込み（crates/workflow-engine/src/ir/expr.rs の `ValueExpr` の Deserialize と
///   `FromRef`・`TemplateExpr` の deny_unknown_fields）に合わせる。

import type { ValueExpr } from "@/generated/workflow-ir";

export type ComposeShape = "object" | "array";

/// 欄が受ける形（値の欄の `shapes`）: 項目のまとまり・リスト・どちらも。
export const OBJECT_SHAPE: readonly ComposeShape[] = ["object"];
export const LIST_SHAPE: readonly ComposeShape[] = ["array"];
export const ANY_SHAPES: readonly ComposeShape[] = ["object", "array"];

/// 組み立てられる入れ物の段の数（欄そのものと、その中の 1 段）。
export const COMPOSE_LEVELS = 2;

/// 値の種類（組み立ての見分けに使う）。`malformed` は `$from` / `$template` のキーがあるのに、エンジンが値式として
/// 読めないもの（保存時の検証が「不正な $from 式」で止める）。
export type ExprKind = "from" | "template" | "malformed" | "object" | "array" | "scalar";

const FROM_KEYS = new Set(["$from", "path", "default"]);
const TEMPLATE_KEYS = new Set(["$template", "vars"]);

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

export function exprKind(v: unknown): ExprKind {
  if (Array.isArray(v)) return "array";
  if (!isPlainObject(v)) return "scalar";
  if ("$from" in v) {
    const ok =
      typeof v.$from === "string" &&
      Object.keys(v).every((k) => FROM_KEYS.has(k)) &&
      (v.path === undefined || v.path === null || typeof v.path === "string");
    return ok ? "from" : "malformed";
  }
  if ("$template" in v) {
    const ok =
      typeof v.$template === "string" &&
      Object.keys(v).every((k) => TEMPLATE_KEYS.has(k)) &&
      (v.vars === undefined || isPlainObject(v.vars));
    return ok ? "template" : "malformed";
  }
  return "object";
}

function children(v: unknown[] | Record<string, unknown>): unknown[] {
  return Array.isArray(v) ? v : Object.values(v);
}

/// 値の中（入れ子を含む）に、入力や結果から選んだ値か差し込みがあるか（形の崩れたものも含む）。
export function containsExpr(v: unknown): boolean {
  const kind = exprKind(v);
  if (kind === "from" || kind === "template" || kind === "malformed") return true;
  if (kind === "scalar") return false;
  return children(v as unknown[] | Record<string, unknown>).some(containsExpr);
}

/// 形の崩れた値式が、値の中（入れ子を含む・値式の中は見ない）にあるか。
function containsMalformed(v: unknown): boolean {
  const kind = exprKind(v);
  if (kind === "malformed") return true;
  if (kind !== "object" && kind !== "array") return false;
  return children(v as unknown[] | Record<string, unknown>).some(containsMalformed);
}

/// 組み立てられない理由: 入れ子が深い・読めない値の指定がある・キーが空の項目がある。
export type ComposeBlock = "deep" | "malformed" | "blank_key";

/// 項目のまとまりかリストの `value` を、段 `level`（欄そのものは 0）の入れ物として行に分けられないときの理由。
/// 分けられるなら null。
export function composeBlock(value: unknown, level: number): ComposeBlock | null {
  const kind = exprKind(value);
  if (kind !== "object" && kind !== "array") return "malformed";
  if (containsMalformed(value)) return "malformed";
  if (level >= COMPOSE_LEVELS) return containsExpr(value) ? "deep" : null;
  const obj = value as unknown[] | Record<string, unknown>;
  // キーが空の項目は行のキーの欄で「未入力」と区別できず、ほかの項目を直したときに落ちる。
  if (!Array.isArray(obj) && Object.hasOwn(obj, "")) return "blank_key";
  for (const c of children(obj)) {
    const k = exprKind(c);
    // 参照も差し込みも無い入れ子は、1 つの値として JSON で直接入力できる（深くてよい）。
    if ((k === "object" || k === "array") && containsExpr(c)) {
      const inner = composeBlock(c, level + 1);
      if (inner) return inner;
    }
  }
  return null;
}

/// 選んでいない値（置いたばかりの欄の `""`・null・未設定）。
export function isBlankValue(value: unknown): boolean {
  return value === undefined || value === null || (typeof value === "string" && value.trim() === "");
}

function isEmptyContainer(v: unknown): boolean {
  return (Array.isArray(v) && v.length === 0) || (isPlainObject(v) && exprKind(v) === "object" && Object.keys(v).length === 0);
}

/// 段 `level` の欄で選べる組み立ての形（`shapes` のうち、その段で入れ物にできるもの）。
export function composeShapesAt(shapes: readonly ComposeShape[], level: number): ComposeShape[] {
  return level < COMPOSE_LEVELS ? [...shapes] : [];
}

/// 欄を開いたときの組み立ての形（組み立てないなら null）。
/// - 行に分けられる項目のまとまり・リストで、欄がその形を受けるなら、その形で開く（空のものは `prefer` のときだけ）。
/// - 空の欄は `prefer`（JSON の項目のまとまりに限る欄・データを追加の値など）の形で開く。
export function initialCompose(
  value: unknown,
  shapes: readonly ComposeShape[],
  level: number,
  prefer?: ComposeShape,
): ComposeShape | null {
  const allowed = composeShapesAt(shapes, level);
  const kind = exprKind(value);
  if ((kind === "object" || kind === "array") && allowed.includes(kind)) {
    if (isEmptyContainer(value)) return prefer === kind ? kind : null;
    return composeBlock(value, level) === null ? kind : null;
  }
  if (isBlankValue(value) && prefer && allowed.includes(prefer)) return prefer;
  return null;
}

/// いまの値を `shape` の行で見せられるか（空の欄は 0 行として見せる）。
export function fitsCompose(value: unknown, shape: ComposeShape, level: number): boolean {
  if (isBlankValue(value)) return true;
  return exprKind(value) === shape && composeBlock(value, level) === null;
}

// ── 行と値の行き来 ──────────────────────────────────────────────

/// 項目のまとまりの 1 行。`value` が undefined の行は、値を入れるまで IR に入れない。
export type ObjectRow = { id: number; key: string; value: ValueExpr | undefined };
/// リストの 1 件。`value` が undefined の件は、値を入れるまで IR に入れない。
export type ListItem = { id: number; value: ValueExpr | undefined };

let nextRowId = 1;
/// 行の識別子（描画のキー。行を足しても消しても、ほかの行の入力の途中の状態を保つ）。
export function rowId(): number {
  nextRowId += 1;
  return nextRowId;
}

/// 項目のまとまりを行に分ける（並びはキーの並び）。
export function objectRows(value: unknown): ObjectRow[] {
  if (!isPlainObject(value) || exprKind(value) !== "object") return [];
  return Object.entries(value).map(([key, v]) => ({ id: rowId(), key, value: v as ValueExpr }));
}

/// 行から項目のまとまりを作る。キーが空の行・同じキーの 2 行目以降・値の無い行は入れない（[`objectRowIssue`]）。
export function objectOfRows(rows: readonly ObjectRow[]): Record<string, ValueExpr> {
  const out: Record<string, ValueExpr> = {};
  for (const r of rows) {
    if (r.key === "" || Object.hasOwn(out, r.key) || r.value === undefined) continue;
    out[r.key] = r.value;
  }
  return out;
}

/// 行が IR に入らない理由（キーが空・ほかの行と同じキー・値が無い）。入るなら null。
export function objectRowIssue(rows: readonly ObjectRow[], i: number): "blank_key" | "duplicate" | "no_value" | null {
  const r = rows[i];
  if (r.key === "") return "blank_key";
  if (rows.slice(0, i).some((x) => x.key === r.key)) return "duplicate";
  if (r.value === undefined) return "no_value";
  return null;
}

/// リストを件に分ける。
export function listItems(value: unknown): ListItem[] {
  if (!Array.isArray(value)) return [];
  return value.map((v) => ({ id: rowId(), value: v as ValueExpr }));
}

/// 件からリストを作る（値の無い件は入れない）。
export function listOfItems(items: readonly ListItem[]): ValueExpr[] {
  return items.filter((x) => x.value !== undefined).map((x) => x.value as ValueExpr);
}

/// キーの並びによらない JSON の文字（同じ値か比べる）。undefined の項目は JSON と同じく落とす。
function stableJson(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map((x) => (x === undefined ? "null" : stableJson(x))).join(",")}]`;
  if (isPlainObject(v)) {
    const entries = Object.keys(v)
      .filter((k) => v[k] !== undefined)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableJson(v[k])}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(v) ?? "undefined";
}

/// 行から作った値 `built` が、いまの値 `value` と同じか（空の欄と、空の入れ物は同じとみなす）。
export function sameComposed(built: unknown, value: unknown): boolean {
  if (isEmptyContainer(built) && isBlankValue(value)) return true;
  return stableJson(built) === stableJson(value);
}
