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

/// 絞り込みに使える列か（並べ替えできる列に加え、複数選択は要素ごとの索引で「含む」を引ける）。
export function isFilterable(c: ColumnModel): boolean {
  if (isSortable(c)) return true;
  const f = c.field;
  return Boolean(f && f.type === "multi_select" && f.materialization !== "per_viewer" && f.materialization !== "volatile");
}

/// 並べ替えできない理由（見出しのクリック・並べ替えの選択肢で案内する）。
export function unsortableReason(c: ColumnModel): string {
  const t = c.field?.type;
  if (t === "multi_select") return "複数選択の列は並べ替えできません（絞り込みはできます）";
  if (t === "record_ref") return "リンクの列は並べ替えできません";
  if (t === "lookup" || t === "rollup" || t === "formula" || t === "computed") {
    return "この派生列は並べ替えできません（索引を付けられる派生列だけが並べ替えに使えます）";
  }
  return "この列は並べ替えできません";
}

/// 直接編集できる列か（派生列・逆向きのリンク・ファイルは読み取り専用）。ユーザー・部署・リンクは候補から選ぶ。
export function isEditable(f: FieldDef | null): boolean {
  if (!f) return false;
  if (f.type === "record_ref") return !f.link?.reverse;
  return ["text", "number", "date", "date_time", "select", "multi_select", "user_ref", "role_ref"].includes(f.type);
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

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
const expFmt = new Intl.NumberFormat("en-US", { notation: "scientific", maximumSignificantDigits: 6 });

/// 日時の表示（年は 4 桁で揃える。0001 年が「1/01/01」と出ていた・D2 P3）。
function formatDateTime(d: Date): string {
  const parts = dateTimeFmt.formatToParts(d);
  return parts.map((p) => (p.type === "year" ? p.value.padStart(4, "0") : p.value)).join("");
}

/// 数値の表示（桁区切り）。桁が多すぎてセルに収まらない値（±1e15 以上・極小値）は指数表記にして、
/// 符号と桁数が見えるようにする（右寄せのセルで左端が切れて符号・桁が消えていた・D-12）。
export function formatNumber(n: number): string {
  const a = Math.abs(n);
  if (a !== 0 && (a >= 1e15 || a < 1e-6)) return expFmt.format(n);
  return numberFmt.format(n);
}

/// 表示用の文字列。
export function formatValue(v: unknown, field: FieldDef | null, key?: string): string {
  if (v === null || v === undefined) return "";
  if (Array.isArray(v)) return v.map((x) => formatValue(x, field)).join(", ");
  const type = field?.type;
  if ((type === "date_time" || key === "_created_at" || key === "_updated_at") && typeof v === "string") {
    const d = new Date(v);
    return Number.isNaN(d.getTime()) ? v : formatDateTime(d);
  }
  if (typeof v === "number") return formatNumber(v);
  if (typeof v === "string" && (type === "number" || field?.result_type === "number") && v !== "") {
    // numeric は文字列で届くことがある（精度を保つ）。表示だけ桁区切りにする。
    const n = Number(v);
    return Number.isFinite(n) && Math.abs(n) < 1e15 ? formatNumber(n) : v.length > 18 && Number.isFinite(n) ? expFmt.format(n) : v;
  }
  if (typeof v === "boolean") return v ? "はい" : "いいえ";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/// スモールセル抑制（PIT-17）で値を伏せたセルの表示（空欄にしない）。
export const SUPPRESSED_LABEL = "件数が少ないため非表示";

/// 1 セルの表示（参照系の列は名前に置き換える・抑制したセルは理由を出す）。
///
/// - `user_ref` 列・作成者列: ディレクトリの表示名（引けなければ id のまま）
/// - リンク列: 参照先のタイトル列（サーバが閲覧者に見える参照先の分だけ `link_labels` に載せる）。タイトルの
///   無い参照先は id の先頭 8 文字
/// - スモールセル抑制に掛かった派生列（`suppressed_fields`）: {@link SUPPRESSED_LABEL}
export function displayCell(
  r: DataRecord,
  c: Pick<ColumnModel, "key" | "field">,
  userName: (id: string) => string | null | undefined,
  roleName: (id: string) => string | null | undefined = () => null,
): { text: string; muted: boolean } {
  const v = cellValue(r, c.key);
  if ((v === null || v === undefined) && (r.suppressed_fields ?? []).includes(c.key)) {
    return { text: SUPPRESSED_LABEL, muted: true };
  }
  const type = c.field?.type;
  const ids = (x: unknown): string[] =>
    (Array.isArray(x) ? x : x == null ? [] : [x]).filter((e): e is string => typeof e === "string");
  if (type === "user_ref" || c.key === "_owner") {
    return { text: ids(v).map((id) => userName(id) || id).join(", "), muted: false };
  }
  if (type === "role_ref") {
    return { text: ids(v).map((id) => roleName(id) || id).join(", "), muted: false };
  }
  if (type === "record_ref") {
    // タイトルの無い参照先（タイトル列が空・逆向きの列の参照元にタイトル列が無い）は id ではなく
    // 「（タイトルなし）」と出す（UUID の先頭が並んで読めなかった・A P3）。
    const labels = (r.link_labels ?? {}) as Record<string, string>;
    const list = ids(v);
    // 逆向きの列はサーバが参照元の見出しを返さないことがある: 1 件も見出しが無ければ件数で出す。
    if (c.field?.link?.reverse && list.length && list.every((id) => !labels[id])) {
      return { text: `${list.length.toLocaleString("ja-JP")} 件のレコード`, muted: true };
    }
    return { text: list.map((id) => labels[id] || UNTITLED).join(", "), muted: false };
  }
  return { text: formatValue(v, c.field, c.key), muted: false };
}

/// タイトルの無い参照先の表示。
export const UNTITLED = "（タイトルなし）";

/// 表示名を引く対象の部署 id（`role_ref` 列の値）。
export function roleRefIds(rows: readonly DataRecord[], cols: readonly ColumnModel[]): string[] {
  const keys = cols.filter((c) => c.field?.type === "role_ref").map((c) => c.key);
  const out = new Set<string>();
  for (const r of rows) {
    for (const k of keys) {
      const v = cellValue(r, k);
      for (const id of Array.isArray(v) ? v : [v]) if (typeof id === "string" && id) out.add(id);
    }
  }
  return [...out];
}

/// 表示名を引く対象の id（`user_ref` 列・作成者列の値）。
export function userRefIds(rows: readonly DataRecord[], cols: readonly ColumnModel[]): string[] {
  const keys = cols.filter((c) => c.field?.type === "user_ref" || c.key === "_owner").map((c) => c.key);
  const out = new Set<string>();
  for (const r of rows) {
    for (const k of keys) {
      const v = cellValue(r, k);
      for (const id of Array.isArray(v) ? v : [v]) if (typeof id === "string" && id) out.add(id);
    }
  }
  return [...out];
}

/// 日付の入力（`2026-10-01`・`2026/10/1`）→ `YYYY-MM-DD`。暦にない日付・0001〜9999 年の外は null。
export function normalizeDate(t: string): string | null {
  const m = /^(\d{4})[-/](\d{1,2})[-/](\d{1,2})$/.exec(t.trim());
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (y < 1 || mo < 1 || mo > 12 || d < 1) return null;
  const dt = new Date(Date.UTC(2000, mo - 1, d));
  dt.setUTCFullYear(y);
  if (dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) return null;
  return `${m[1]}-${String(mo).padStart(2, "0")}-${String(d).padStart(2, "0")}`;
}

/// 絞り込み・書式の比較値の検査（列の型に合わない値はサーバへ送らない・A-9）。合えば送る値を返す。
export function parseFilterValue(
  text: string,
  col: Pick<ColumnModel, "key" | "field"> | undefined,
  op: CondOp,
): { ok: true; value: unknown } | { ok: false; error: string } {
  if (op === "exists" || op === "is_null") return { ok: true, value: undefined };
  const t = text.trim();
  if (t === "") return { ok: false, error: "値を入れてください" };
  const type = col?.field?.type ?? (col?.key === "_created_at" || col?.key === "_updated_at" ? "date_time" : undefined);
  const resultType = col?.field?.result_type;
  if (type === "number" || resultType === "number") {
    const n = Number(t.replace(/[,，\s]/g, ""));
    return Number.isFinite(n) ? { ok: true, value: n } : { ok: false, error: "数値を入れてください" };
  }
  if (type === "date" || resultType === "date") {
    const d = normalizeDate(t);
    return d ? { ok: true, value: d } : { ok: false, error: "日付を選んでください（例: 2026-10-01）" };
  }
  if (type === "date_time" || resultType === "date_time") {
    // datetime-local の値（タイムゾーンなし）は手元の時刻として UTC に直す。
    const dt = new Date(t);
    return Number.isNaN(dt.getTime()) ? { ok: false, error: "日時を選んでください" } : { ok: true, value: dt.toISOString() };
  }
  return { ok: true, value: t };
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
      const plain = t.replace(/[,，\s]/g, "");
      const n = Number(plain);
      if (!/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(plain) || !Number.isFinite(n)) {
        return { ok: false, error: "数値を入力してください" };
      }
      // 有効桁が 15 桁を超える値は数値として正確に保てない（黙って丸めずに知らせる・A-15）。
      const digits = plain.replace(/[eE].*$/, "").replace(/^[+-]/, "").replace(".", "").replace(/^0+/, "");
      if (digits.replace(/0+$/, "").length > 15) {
        return { ok: false, error: "有効桁が 15 桁を超える数値は正確に保存できません（桁を減らすか、テキストの列を使ってください）" };
      }
      return { ok: true, value: n };
    }
    case "date": {
      const d = normalizeDate(t);
      return d ? { ok: true, value: d } : { ok: false, error: "日付は 2026-10-01 の形で入力してください（0001〜9999 年）" };
    }
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
    case "user_ref":
    case "role_ref":
    case "record_ref": {
      // 候補から選んだ値（{ids, labels} の JSON）か、id の貼り付け（カンマ区切り）。
      let ids: string[];
      if (t.startsWith("{")) {
        try {
          ids = ((JSON.parse(t) as { ids?: unknown }).ids as string[]) ?? [];
        } catch {
          return { ok: false, error: "値を読めませんでした" };
        }
      } else {
        ids = t.split(/[,、\s]+/).filter(Boolean);
        if (field.type !== "role_ref" && ids.some((x) => !UUID_RE.test(x))) {
          return { ok: false, error: "候補から選んでください（Enter で候補の一覧を開きます）" };
        }
      }
      if (field.type === "record_ref") return { ok: true, value: ids };
      return { ok: true, value: ids[0] ?? null };
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
  ends_with: "が次で終わる",
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
    // 日時の値（UTC の ISO）は datetime-local の入力欄に合わせて手元の時刻へ直して見せる。
    const last = rows[rows.length - 1];
    if (typeof l.cmp.value === "string" && /T\d{2}:\d{2}/.test(l.cmp.value) && /Z|[+-]\d{2}:?\d{2}$/.test(l.cmp.value)) {
      last.value = toLocalInput(l.cmp.value);
    }
  }
  return { rows, advanced: null };
}

export function buildFilter(rows: FilterRow[], advanced: Condition | null, columns: ColumnModel[]): Condition | null {
  const leaves: Condition[] = [];
  for (const r of rows) {
    const col = columns.find((c) => c.key === r.field);
    if (NO_VALUE.includes(r.op)) {
      leaves.push({ cmp: { field: r.field, op: r.op } });
      continue;
    }
    // 型に合わない値の条件は送らない（画面で弾いているが、保存ビューの古い条件も含めて守る）。
    const v = parseFilterValue(r.value, col, r.op);
    if (v.ok) leaves.push({ cmp: { field: r.field, op: r.op, value: v.value } });
  }
  if (advanced) leaves.push(advanced);
  if (leaves.length === 0) return null;
  return leaves.length === 1 ? leaves[0] : { all: leaves };
}

/// ISO の日時 → datetime-local の入力値（手元の時刻・分まで）。
export function toLocalInput(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${String(d.getFullYear()).padStart(4, "0")}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/// 表示名から参照名を作る（英数字だけなら小文字のスネークケース、そうでなければ連番）。
export function keyFromLabel(label: string, taken: ReadonlySet<string>): string {
  const ascii = label
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  let base = /^[a-z][a-z0-9_]*$/.test(ascii) ? ascii.slice(0, 48) : "col";
  if (base === "col" || taken.has(base)) {
    let i = 1;
    while (taken.has(`${base}_${i}`)) i += 1;
    base = `${base}_${i}`;
  }
  return base;
}

/// サーバの文言の列の参照名（`'amount'`・`"amount"`）を表示名（「金額」）に直す。サーバは列を参照名で書くので、
/// 日本語の表示名の列が `col_3` のように見えていた（A P3）。閲覧者が読める列（`fields`）だけを直す。
export function withFieldLabels(message: string, fields: readonly Pick<FieldDef, "key" | "display_name">[]): string {
  let out = message;
  // 長い参照名から置き換える（`col_1` が `col_10` の一部を食わない）。
  const sorted = [...fields].filter((f) => f.display_name && f.display_name !== f.key).sort((a, b) => b.key.length - a.key.length);
  for (const f of sorted) {
    const k = f.key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    out = out.replace(new RegExp(`\\s?['"‘’“”]${k}['"‘’“”]\\s?`, "g"), `「${f.display_name}」`);
  }
  return out;
}

/// 列の参照名 → 見出し（表示名・無ければ参照名）。
export function fieldLabel(fields: readonly Pick<FieldDef, "key" | "display_name">[], key: string): string {
  return fields.find((f) => f.key === key)?.display_name || key;
}

/// `base` を参照名の規則（`^[a-z][a-z0-9_]{0,63}$`）に収め、`taken` と重ならないよう `_2`, `_3`… を付ける。
export function uniqueKey(base: string, taken: ReadonlySet<string>): string {
  let b = base.toLowerCase().replace(/[^a-z0-9_]+/g, "_").replace(/^[^a-z]+/, "") || "col";
  b = b.slice(0, 60);
  if (!taken.has(b)) return b;
  let i = 2;
  while (taken.has(`${b}_${i}`)) i += 1;
  return `${b}_${i}`;
}

/// 既定の表示設定（全列を表示する表）。
export function defaultSpec(): ViewSpec {
  return { kind: "grid", columns: [], row_height: "short" };
}
