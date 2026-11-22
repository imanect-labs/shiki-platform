/// 業務データの CSV（書き出しと、CSV からのテーブル作成）。
///
/// どちらもサーバの既存の経路だけを使う（新しい権限の経路を作らない）:
/// - 書き出しはクエリ API のスナップショット読取（data-platform §3.3・エクスポート用）を辿る。行述語・列のマスクは
///   サーバが閲覧者本人の権限で合成済みなので、ここでは返ってきたものを並べるだけ。
/// - 作成はテーブルの作成 API ＋ CSV の一括取込 API（§16.7）。型・必須などの検証は取込のジョブが行う。
///
/// 書き出した CSV は取込（`crates/data/src/bulk/csv.rs`）とそのまま往復できる形で書く:
/// 見出しは表示名（同じ表示名が複数あれば参照名）、multi_select / record_ref は `;` 区切り、空は値なし。

import { runQuery, type DataRecord, type DataTable, type FieldDef, type FieldType } from "@/lib/data-api";
import { keyFromLabel } from "@/lib/data-view";

/// 取込が受け付ける CSV の上限（サーバの `MAX_BULK_CSV_BYTES` と同じ）。
export const MAX_CSV_BYTES = 32 * 1024 * 1024;
/// 1 テーブルの列の上限（サーバの `MAX_FIELDS` と同じ）。
export const MAX_CSV_COLUMNS = 200;
/// 書き出しの 1 ページの件数（クエリ API の上限）。
const EXPORT_PAGE = 200;

// ---------------------------------------------------------------------------
// CSV の基本
// ---------------------------------------------------------------------------

/// RFC4180 のクォート（`,` `"` 改行を含むセルだけ囲む）。
export function csvCell(s: string): string {
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

/// 行を CSV にする（CRLF 区切り・末尾改行あり。Excel が素直に開ける形）。
export function csvLines(rows: readonly (readonly string[])[]): string {
  return rows.map((r) => r.map(csvCell).join(",")).join("\r\n") + "\r\n";
}

/// ファイル名に使えない文字を `_` にする。
export function safeFileName(name: string, fallback = "table"): string {
  return name.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_").trim() || fallback;
}

// ---------------------------------------------------------------------------
// 書き出し
// ---------------------------------------------------------------------------

/// 取込の行キーの見出し（`crates/data/src/bulk/csv.rs` の `ROW_KEY_COLUMN`）。
const ROW_KEY_HEADER = "_key";
/// 表計算ソフトが数式として解釈する先頭文字（CSV インジェクション）。
const FORMULA_LEAD = /^[=+\-@\t\r]/;

/// JSON の値 1 つをセルの文字列にする（取込と往復できる形）。
function scalar(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}

/// レコードの値 1 つ → セル。`numeric` は数値の列の値か（数式の対策の対象外）。
export function exportCell(v: unknown, numeric: boolean): string {
  if (v == null) return "";
  let s: string;
  if (Array.isArray(v)) {
    const parts = v.map(scalar);
    // `;` 区切りで往復できない要素があれば JSON 配列で書く（取込は `[` で始まれば JSON 配列として読み、
    // それ以外は `;` で割って各要素の前後の空白を除き、空の要素を捨てる）。
    const plain =
      v.every((x) => x == null || typeof x !== "object") &&
      parts.every((p) => p !== "" && p === p.trim() && !p.includes(";")) &&
      !parts[0]?.startsWith("[");
    s = plain ? parts.join(";") : JSON.stringify(v);
  } else {
    s = scalar(v);
  }
  if (numeric && typeof v === "number") return s;
  // 他人の書いた値が表計算ソフトで数式として動かないよう、先頭に `'` を付ける。
  return FORMULA_LEAD.test(s) ? `'${s}` : s;
}

/// 書き出す列（読める列・表示順）と見出し。表示名で取込が同じ列へ照合できないときは参照名を見出しにする:
/// 表示名が重複する・別の列の参照名と同じ（取込は参照名を先に照合する）・行キーの見出し `_key`・
/// 前後に空白がある（取込は見出しの空白を除く）・表計算ソフトが数式と読む先頭文字。参照名は英小文字・数字・`_` だけ。
export function exportColumns(table: DataTable, masked: ReadonlySet<string>): { field: FieldDef; header: string }[] {
  const fields = [...table.schema.fields]
    .filter((f) => !masked.has(f.key))
    .sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
  const keys = new Set(table.schema.fields.map((f) => f.key));
  const count = new Map<string, number>();
  for (const f of fields) {
    const d = f.display_name || f.key;
    count.set(d, (count.get(d) ?? 0) + 1);
  }
  return fields.map((f) => {
    const d = f.display_name || f.key;
    const unsafe =
      (count.get(d) ?? 0) > 1 ||
      (d !== f.key && keys.has(d)) ||
      d === ROW_KEY_HEADER ||
      d !== d.trim() ||
      FORMULA_LEAD.test(d);
    return { field: f, header: unsafe ? f.key : d };
  });
}

/// レコード 1 件 → CSV の 1 行。
export function exportRow(rec: DataRecord, cols: readonly { field: FieldDef }[]): string[] {
  const data = (rec.data ?? {}) as Record<string, unknown>;
  const suppressed = new Set(rec.suppressed_fields ?? []);
  return cols.map(({ field }) =>
    suppressed.has(field.key) ? "" : exportCell(data[field.key], field.type === "number"),
  );
}

/// テーブルの全行（閲覧者が読める分）を CSV にする。スナップショット読取なので、書き出し中の書込で
/// 行が重複・欠落しない。`onProgress` には書き出した行数を渡す。
export async function exportTableCsv(
  table: DataTable,
  masked: ReadonlySet<string>,
  opts: { signal?: AbortSignal; onProgress?: (rows: number) => void } = {},
): Promise<{ blob: Blob; rows: number }> {
  const cols = exportColumns(table, masked);
  const chunks: string[] = ["﻿", csvLines([cols.map((c) => c.header)])];
  let cursor: string | undefined;
  let rows = 0;
  do {
    const r = await runQuery(
      table.id,
      { snapshot: true, limit: EXPORT_PAGE, cursor },
      { signal: opts.signal },
    );
    const items = r.items ?? [];
    if (items.length) chunks.push(csvLines(items.map((rec) => exportRow(rec, cols))));
    rows += items.length;
    opts.onProgress?.(rows);
    cursor = r.next_cursor ?? undefined;
  } while (cursor);
  return { blob: new Blob(chunks, { type: "text/csv;charset=utf-8" }), rows };
}

// ---------------------------------------------------------------------------
// CSV からのテーブル作成
// ---------------------------------------------------------------------------

/// CSV から作るときに選べる型。
export type CsvColumnType = Extract<FieldType, "text" | "number" | "date" | "date_time">;

export const CSV_COLUMN_TYPES: { value: CsvColumnType; label: string }[] = [
  { value: "text", label: "テキスト" },
  { value: "number", label: "数値" },
  { value: "date", label: "日付" },
  { value: "date_time", label: "日時" },
];

/// 取込が数値として読む形（JSON の数値）。
const NUMBER_RE = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?$/;
/// 日付として受ける形（`2024-01-05`・`2024/1/5`）。送るときは `YYYY-MM-DD` に直す。
const DATE_RE = /^(\d{4})[-/](\d{1,2})[-/](\d{1,2})$/;
/// 日時（RFC3339・秒まで必須）。
const DATE_TIME_RE = /^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/;

/// 日付のセルを `YYYY-MM-DD` に直す（日付として読めなければ null）。
export function normalizeDate(s: string): string | null {
  const m = DATE_RE.exec(s.trim());
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const dt = new Date(Date.UTC(y, mo - 1, d));
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) return null;
  return `${m[1]}-${String(mo).padStart(2, "0")}-${String(d).padStart(2, "0")}`;
}

function isDateTime(s: string): boolean {
  return DATE_TIME_RE.test(s) && !Number.isNaN(Date.parse(s.replace(" ", "T")));
}

/// 列の値（空を除く）から型を推定する。全部が同じ形のときだけその型にし、空の列・混在はテキスト。
export function inferColumnType(values: readonly string[]): CsvColumnType {
  const vs = values.map((v) => v.trim()).filter((v) => v !== "");
  if (!vs.length) return "text";
  if (vs.every((v) => NUMBER_RE.test(v))) return "number";
  if (vs.every((v) => normalizeDate(v) != null)) return "date";
  if (vs.every(isDateTime)) return "date_time";
  return "text";
}

/// CSV から作る列 1 つ（画面で名前・型を直せる）。
export type CsvColumnPlan = {
  /// 元の CSV の見出し。
  header: string;
  name: string;
  type: CsvColumnType;
  /// 推定に使った先頭の値（画面の例示）。
  samples: string[];
};

export type CsvTablePlan = {
  columns: CsvColumnPlan[];
  /// 見出しを除いたデータの行。
  rows: string[][];
};

/// パース済みの CSV（先頭が見出し）→ 作成の計画。形の誤りは利用者向けの文言で投げる。
export function planFromCsv(parsed: string[][]): CsvTablePlan {
  const [head, ...rows] = parsed;
  if (!head || head.every((h) => h.trim() === "")) throw new Error("見出しの行がありません。1 行目に列名を書いてください。");
  if (head.length > MAX_CSV_COLUMNS) throw new Error(`列が多すぎます（最大 ${MAX_CSV_COLUMNS} 列）。`);
  if (!rows.length) throw new Error("データの行がありません。見出しの下に 1 行以上書いてください。");
  const bad = rows.findIndex((r) => r.length !== head.length);
  if (bad >= 0) {
    throw new Error(
      `CSV の ${bad + 2} 行目の列の数が見出しと合いません（見出しは ${head.length} 列・この行は ${rows[bad].length} 列）。`,
    );
  }
  const used = new Set<string>();
  const columns = head.map((h, i) => {
    let name = h.trim() || `列${i + 1}`;
    // 同じ列名は作れても見分けにくいので、2 つ目以降に番号を付ける。
    if (used.has(name)) {
      let n = 2;
      while (used.has(`${name} (${n})`)) n += 1;
      name = `${name} (${n})`;
    }
    used.add(name);
    const values = rows.map((r) => r[i] ?? "");
    return {
      header: h,
      name,
      type: inferColumnType(values),
      samples: values.filter((v) => v.trim() !== "").slice(0, 3),
    };
  });
  return { columns, rows };
}

/// 計画 → テーブルの列定義と、見出しを参照名に置き換えた取込用の CSV（日付は `YYYY-MM-DD` に直す）。
/// 最初の列は索引付き（既定の並べ替え・絞り込みにすぐ使える・通常の作成と同じ）。
export function buildCsvTable(plan: CsvTablePlan): { fields: FieldDef[]; csv: string } {
  const taken = new Set<string>();
  const fields = plan.columns.map((c, i) => {
    const key = keyFromLabel(c.name, taken);
    taken.add(key);
    return { key, display_name: c.name.trim() || key, type: c.type, indexed: i === 0 } as FieldDef;
  });
  const body = plan.rows.map((r) =>
    r.map((v, i) => {
      if (plan.columns[i].type === "date") return normalizeDate(v) ?? v;
      // 型を推定したときと同じく前後の空白を除いて送る（取込の検証は数値・日時の空白を許さない）。
      if (plan.columns[i].type === "number" || plan.columns[i].type === "date_time") return v.trim();
      return v;
    }),
  );
  return { fields, csv: csvLines([fields.map((f) => f.key), ...body]) };
}

/// ファイル名から既定のテーブル名（拡張子を除く）。
export function tableNameFromFile(fileName: string): string {
  return fileName.replace(/\.(csv|txt)$/i, "").trim().slice(0, 128);
}
