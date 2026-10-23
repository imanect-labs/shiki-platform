/// 構造化データの表示の純粋な部品（Task 15.5）: 列のモデル・値の表示/入力の変換・条件付き書式の評価。
///
/// 型は `@/lib/data-api`（= 生成型）。行・列の権限はサーバ側（`run_query`）が閲覧者本人の権限で掛けるので、
/// ここは「どう見せるか」だけを扱う。

import type {
  ColumnConfig,
  CondOp,
  Condition,
  DataRecord,
  FieldDef,
  FieldType,
  FormatColor,
  GridView,
  SortKey,
  ViewSpec,
} from "@/lib/data-api";

/// 擬似列（全行にあるシステム列）。
export const SYSTEM_COLUMNS: { key: string; label: string }[] = [
  { key: "_updated_at", label: "更新日時" },
  { key: "_created_at", label: "作成日時" },
  { key: "_owner", label: "作成者" },
];

export const FIELD_TYPE_LABELS: Record<FieldType, string> = {
  text: "テキスト",
  number: "数値",
  date: "日付",
  date_time: "日時",
  select: "単一選択",
  multi_select: "複数選択",
  user_ref: "ユーザー",
  role_ref: "部署",
  file_ref: "ファイル",
  record_ref: "リンク",
  lookup: "ルックアップ",
  computed: "計算",
  rollup: "ロールアップ",
  formula: "数式",
};

/// 表示する 1 列。
export type ColumnModel = {
  key: string;
  /// 見出し（ビューの上書き → スキーマの表示名 → 参照名）。
  label: string;
  /// スキーマの表示名（ビューの上書きが無いときの見出し）。
  baseLabel: string;
  width: number;
  frozen: boolean;
  hidden: boolean;
  sortBy: string | null;
  field: FieldDef | null;
  system: boolean;
};

export const DEFAULT_COLUMN_WIDTH = 160;

/// スキーマ＋ビューの列設定 → 表示順の列（読めない列は除く・非表示も含む）。
export function resolveColumns(
  fields: FieldDef[],
  grid: GridView | null,
  masked: ReadonlySet<string>,
): ColumnModel[] {
  const byKey = new Map(fields.map((f) => [f.key, f]));
  const sys = new Map(SYSTEM_COLUMNS.map((s) => [s.key, s.label]));
  const configs = grid?.columns ?? [];
  const seen = new Set<string>();
  const out: ColumnModel[] = [];
  const push = (key: string, cfg: ColumnConfig | null, hiddenDefault: boolean) => {
    if (seen.has(key) || masked.has(key)) return;
    const field = byKey.get(key) ?? null;
    const sysLabel = sys.get(key);
    if (!field && !sysLabel) return;
    seen.add(key);
    const baseLabel = field ? field.display_name || field.key : (sysLabel ?? key);
    out.push({
      key,
      label: cfg?.label || baseLabel,
      baseLabel,
      width: cfg?.width ?? DEFAULT_COLUMN_WIDTH,
      frozen: cfg?.frozen ?? false,
      hidden: cfg ? (cfg.hidden ?? false) : hiddenDefault,
      sortBy: cfg?.sort_by ?? null,
      field,
      system: !field,
    });
  };
  for (const c of configs) push(c.field, c, false);
  const ordered = [...fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
  for (const f of ordered) push(f.key, null, false);
  // 擬似列は設定に無ければ非表示で末尾に置く（列メニューから出せる）。
  for (const s of SYSTEM_COLUMNS) push(s.key, null, true);
  return out;
}

/// 列のモデル → ビューの列設定（既定値は省く）。
export function columnsToConfig(cols: ColumnModel[]): ColumnConfig[] {
  return cols.map((c) => {
    const cfg: ColumnConfig = { field: c.key };
    if (c.label !== c.baseLabel) cfg.label = c.label;
    if (c.width !== DEFAULT_COLUMN_WIDTH) cfg.width = Math.round(Math.min(1200, Math.max(40, c.width)));
    if (c.hidden) cfg.hidden = true;
    if (c.frozen && !c.hidden) cfg.frozen = true;
    if (c.sortBy) cfg.sort_by = c.sortBy;
    return cfg;
  });
}

/// サーバの `effective_sort` と同じ規則（グループ化の列を並べ替えの先頭へ・最大 4 キー）。
export function effectiveSort(spec: ViewSpec, sort: SortKey[]): SortKey[] {
  if (spec.kind !== "grid" || !spec.group_by?.length) return sort;
  const out: SortKey[] = spec.group_by.map((g) => ({ field: g.field, descending: g.descending ?? false }));
  for (const k of sort) if (!out.some((x) => x.field === k.field)) out.push(k);
  return out.slice(0, 4);
}

/// 並べ替えに使える列か（閲覧者ごと・時刻依存の派生列と複数値の列は索引が無い）。
export function isSortable(c: ColumnModel): boolean {
  if (c.system) return true;
  const f = c.field!;
  if (f.materialization === "per_viewer" || f.materialization === "volatile") return false;
  if (f.type === "multi_select" || f.type === "record_ref") return false;
  if (f.type === "lookup" || f.type === "rollup" || f.type === "formula" || f.type === "computed") {
    return f.materialization === "invariant" && Boolean(f.indexed);
  }
  return true;
}

/// 直接編集できる列か（派生列・参照系は読み取り専用）。
export function isEditable(f: FieldDef | null): boolean {
  if (!f) return false;
  return ["text", "number", "date", "date_time", "select", "multi_select"].includes(f.type);
}

/// レコードの列の値（擬似列はレコードのメタ）。
export function cellValue(r: DataRecord, key: string): unknown {
  if (key === "_owner") return r.owner;
  if (key === "_created_at") return r.created_at;
  if (key === "_updated_at") return r.updated_at;
  const data = (r.data ?? {}) as Record<string, unknown>;
  return data[key];
}

const dateTimeFmt = new Intl.DateTimeFormat("ja-JP", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});
const numberFmt = new Intl.NumberFormat("ja-JP", { maximumFractionDigits: 10 });

/// 表示用の文字列。
export function formatValue(v: unknown, field: FieldDef | null, key?: string): string {
  if (v === null || v === undefined) return "";
  if (Array.isArray(v)) return v.map((x) => formatValue(x, field)).join(", ");
  const type = field?.type;
  if ((type === "date_time" || key === "_created_at" || key === "_updated_at") && typeof v === "string") {
    const d = new Date(v);
    return Number.isNaN(d.getTime()) ? v : dateTimeFmt.format(d);
  }
  if (typeof v === "number") return numberFmt.format(v);
  if (typeof v === "string" && (type === "number" || field?.result_type === "number") && v !== "") {
    // numeric は文字列で届くことがある（精度を保つ）。表示だけ桁区切りにする。
    const n = Number(v);
    return Number.isFinite(n) && Math.abs(n) < 1e15 ? numberFmt.format(n) : v;
  }
  if (typeof v === "boolean") return v ? "はい" : "いいえ";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/// 入力の文字列 → 保存する値（型の検査は画面でも行い、正はサーバの検証器）。
export function parseInput(
  text: string,
  field: FieldDef,
): { ok: true; value: unknown } | { ok: false; error: string } {
  const t = text.trim();
  if (t === "") return { ok: true, value: null };
  switch (field.type) {
    case "number": {
      const n = Number(t.replace(/,/g, ""));
      return Number.isFinite(n) ? { ok: true, value: n } : { ok: false, error: "数値を入力してください" };
    }
    case "date":
      return /^\d{4}-\d{2}-\d{2}$/.test(t)
        ? { ok: true, value: t }
        : { ok: false, error: "日付は YYYY-MM-DD で入力してください" };
    case "date_time": {
      const d = new Date(t);
      return Number.isNaN(d.getTime())
        ? { ok: false, error: "日時を入力してください（例: 2026-10-01 09:00）" }
        : { ok: true, value: d.toISOString() };
    }
    case "select":
      return (field.options ?? []).includes(t)
        ? { ok: true, value: t }
        : { ok: false, error: `選択肢から選んでください（${(field.options ?? []).join(" / ")}）` };
    case "multi_select": {
      const parts = t.split(/[,、]/).map((s) => s.trim()).filter(Boolean);
      const bad = parts.filter((p) => !(field.options ?? []).includes(p));
      return bad.length
        ? { ok: false, error: `選択肢にない値です: ${bad.join(", ")}` }
        : { ok: true, value: parts };
    }
    default:
      return { ok: true, value: t };
  }
}

// ---------------------------------------------------------------------------
// 条件付き書式（クエリ IR の Condition と同じ 2 値論理・query-optimizer.md §3.1）
// ---------------------------------------------------------------------------

function isMissing(v: unknown): boolean {
  return v === null || v === undefined || v === "" || (Array.isArray(v) && v.length === 0);
}

function scalarEq(a: unknown, b: unknown): boolean {
  if (typeof a === "number" || typeof b === "number") {
    const x = Number(a);
    const y = Number(b);
    return Number.isFinite(x) && Number.isFinite(y) && x === y;
  }
  return String(a) === String(b);
}

function compare(a: unknown, b: unknown): number | null {
  const x = Number(a);
  const y = Number(b);
  if (typeof b === "number" && Number.isFinite(x)) return x - y;
  if (typeof a === "string" && typeof b === "string") return a < b ? -1 : a > b ? 1 : 0;
  return Number.isFinite(x) && Number.isFinite(y) ? x - y : null;
}

/// 条件を 1 行の値で評価する（値なしの `neq` は真・`not` は補集合・多値列の `eq` は「含む」）。
export function evalCondition(c: Condition, r: DataRecord): boolean {
  if ("all" in c) return c.all.every((x) => evalCondition(x, r));
  if ("any" in c) return c.any.some((x) => evalCondition(x, r));
  if ("not" in c) return !evalCondition(c.not, r);
  if (!("cmp" in c)) return false;
  const { field, op, value } = c.cmp;
  const v = cellValue(r, field);
  const values = Array.isArray(v) ? v : [v];
  switch (op) {
    case "exists":
      return !isMissing(v);
    case "is_null":
      return isMissing(v);
    case "eq":
      return !isMissing(v) && values.some((x) => scalarEq(x, value));
    case "neq":
      return isMissing(v) || !values.some((x) => scalarEq(x, value));
    case "in":
      return !isMissing(v) && Array.isArray(value) && values.some((x) => value.some((y) => scalarEq(x, y)));
    case "lt":
    case "lte":
    case "gt":
    case "gte": {
      if (isMissing(v)) return false;
      return values.some((x) => {
        const d = compare(x, value);
        if (d === null) return false;
        return op === "lt" ? d < 0 : op === "lte" ? d <= 0 : op === "gt" ? d > 0 : d >= 0;
      });
    }
    case "contains":
    case "starts_with":
    case "ends_with": {
      if (isMissing(v) || typeof value !== "string") return false;
      const needle = value.toLowerCase();
      return values.some((x) => {
        const s = String(x).toLowerCase();
        return op === "contains" ? s.includes(needle) : op === "starts_with" ? s.startsWith(needle) : s.endsWith(needle);
      });
    }
    default:
      return false;
  }
}

/// 条件付き書式の色の CSS（テーマのトークンに寄せた淡い塗り・文字色）。
export const FORMAT_COLORS: Record<FormatColor, { label: string; bg: string; fg: string }> = {
  red: { label: "赤", bg: "color-mix(in oklab, var(--destructive) 16%, var(--card))", fg: "var(--destructive)" },
  orange: { label: "橙", bg: "color-mix(in oklab, #f59e0b 20%, var(--card))", fg: "#b45309" },
  yellow: { label: "黄", bg: "color-mix(in oklab, #eab308 22%, var(--card))", fg: "#a16207" },
  green: { label: "緑", bg: "color-mix(in oklab, #22c55e 18%, var(--card))", fg: "#15803d" },
  blue: { label: "青", bg: "color-mix(in oklab, var(--primary) 16%, var(--card))", fg: "var(--primary)" },
  purple: { label: "紫", bg: "color-mix(in oklab, #a855f7 16%, var(--card))", fg: "#7e22ce" },
  gray: { label: "灰", bg: "var(--muted)", fg: "var(--muted-foreground)" },
};

/// 行・列に掛かる書式（上から最初に一致した規則）。
export function formatFor(
  grid: GridView | null,
  r: DataRecord,
  key: string,
): { color: FormatColor; bold: boolean } | null {
  for (const f of grid?.conditional_format ?? []) {
    if (f.fields?.length && !f.fields.includes(key)) continue;
    if (evalCondition(f.condition, r)) return { color: f.color, bold: f.bold ?? false };
  }
  return null;
}

/// 比較演算子の日本語。
export const OP_LABELS: Partial<Record<string, string>> = {
  eq: "が次と等しい",
  neq: "が次と等しくない",
  lt: "が次より小さい",
  lte: "が次以下",
  gt: "が次より大きい",
  gte: "が次以上",
  contains: "が次を含む",
  starts_with: "が次で始まる",
  exists: "が空でない",
  is_null: "が空",
};

/// 絞り込みの 1 条件（画面で編集する形・すべて AND で結ぶ）。
export type FilterRow = { field: string; op: CondOp; value: string };

const NO_VALUE: string[] = ["exists", "is_null"];

/// 保存された条件 → 画面の条件行（AND の比較の並びだけ編集できる。それ以外は「詳細な条件」として残す）。
export function splitFilter(c: Condition | null | undefined): { rows: FilterRow[]; advanced: Condition | null } {
  if (!c) return { rows: [], advanced: null };
  const leaves = "all" in c ? c.all : [c];
  const rows: FilterRow[] = [];
  for (const l of leaves) {
    if (!("cmp" in l) || (l.cmp.op === "in") || Array.isArray(l.cmp.value)) return { rows: [], advanced: c };
    rows.push({ field: l.cmp.field, op: l.cmp.op, value: l.cmp.value == null ? "" : String(l.cmp.value) });
  }
  return { rows, advanced: null };
}

export function buildFilter(rows: FilterRow[], advanced: Condition | null, columns: ColumnModel[]): Condition | null {
  const leaves: Condition[] = rows.map((r) => {
    const col = columns.find((c) => c.key === r.field);
    const numeric = col?.field?.type === "number";
    const value = NO_VALUE.includes(r.op) ? undefined : numeric ? Number(r.value) : r.value;
    return { cmp: { field: r.field, op: r.op, value } };
  });
  if (advanced) leaves.push(advanced);
  if (leaves.length === 0) return null;
  return leaves.length === 1 ? leaves[0] : { all: leaves };
}

/// 既定の表示設定（全列を表示する表）。
export function defaultSpec(): ViewSpec {
  return { kind: "grid", columns: [], row_height: "short" };
}
