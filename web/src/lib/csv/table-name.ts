/// 複数 CSV クエリ（JOIN）の追加テーブル名（#1037）。
///
/// 規則は backend の `tabular::is_valid_table_name` と同じ（英小文字・数字・`_`、先頭は数字以外、
/// 63 文字以内）。`data` は主ファイルに予約。最終判定は backend が行い、ここは入力補助に留める。

export const PRIMARY_TABLE = "data";
/// 追加できるテーブル数の上限（backend の `MAX_EXTRA_TABLES` と同じ）。
export const MAX_EXTRA_TABLES = 8;

const TABLE_NAME_RE = /^[a-z_][a-z0-9_]{0,62}$/;

export function isValidTableName(name: string): boolean {
  return TABLE_NAME_RE.test(name) && name !== PRIMARY_TABLE;
}

/// ファイル名からテーブル名の候補を作る（`売上 2024.csv` → `t1` / `Customers-Master.csv` →
/// `customers_master`）。既に使われている名前（`taken`）と `data` は避けて連番を付ける。
export function deriveTableName(fileName: string, taken: Iterable<string>): string {
  const used = new Set(taken);
  used.add(PRIMARY_TABLE);
  let base = fileName
    .replace(/\.csv$/i, "")
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 56);
  // 英字を含まない名前（`0顧客リスト` → `0` など）は意味のある名前にならないので連番に回す。
  if (!/[a-z]/.test(base)) base = "";
  if (base && /^[0-9]/.test(base)) base = `t_${base}`;
  if (!base) {
    // 英字が残らない名前（日本語など）は t1, t2, … を振る。
    for (let i = 1; ; i++) {
      if (!used.has(`t${i}`)) return `t${i}`;
    }
  }
  if (!used.has(base)) return base;
  for (let i = 2; ; i++) {
    const candidate = `${base}_${i}`;
    if (!used.has(candidate)) return candidate;
  }
}
