/// CSV クエリ/パッチ API クライアント（Task 11P.7/11P.8）。型は OpenAPI 生成。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type SchemaResponse = components["schemas"]["SchemaResponse"];
export type TableResponse = components["schemas"]["TableResponse"];
export type PatchResponse = components["schemas"]["PatchResponse"];
export type SaveResponse = components["schemas"]["SaveResponse"];
/// 同じ名前のファイルがあるときの扱い（`fail` は 409・`add_number` はサーバーが番号を付けて保存する）。
export type OnNameTaken = components["schemas"]["OnNameTaken"];
type ConflictCode = components["schemas"]["ConflictCode"];

/// パッチ操作（backend の tabular::PatchOp と 1:1・タグ付き union）。
export type PatchOp =
  | { op: "cell_update"; row: number; col: number; value: string }
  | { op: "row_insert"; at: number; values: string[] }
  | { op: "row_delete"; row: number }
  | { op: "column_add"; name: string }
  | { op: "column_delete"; col: number }
  | { op: "column_rename"; col: number; name: string };

/// 楽観ロック競合（409・編集の重なり）。UI はリロードを促す。新規保存の名前の衝突は [`CsvNameTaken`]。
export class TabularConflict extends Error {
  constructor(
    public baseRev: number,
    public currentRev: number,
  ) {
    super("CSV が他の編集で更新されました");
    this.name = "TabularConflict";
  }
}

/// 構造化 400（`invalid_input`）の利用者向けの文言（文字コードを読み取れない CSV など・GAP-45）。無ければ null。
async function invalidInputMessage(res: Response): Promise<string | null> {
  if (res.status !== 400) return null;
  const body = (await res.clone().json().catch(() => null)) as { code?: string; message?: string } | null;
  return body?.code === "invalid_input" && body.message ? body.message : null;
}

/// CSV の列や行を読み込めなかった誤り。`message` は利用者に見せてよい文言。
///
/// - `notFound`: 無いか、見る権限が無い（404・403）。画面は「見つかりません」と言う。
/// - `unreadable`: CSV として読めない（文字コードを読み取れないなど・`invalid_input`）。`message` がその理由と直し方。
/// - どちらでもなければ、サーバーか通信の問題（読み込み直せば通ることがある）。
export class TabularLoadError extends Error {
  constructor(
    message: string,
    public readonly notFound: boolean,
    public readonly unreadable: boolean,
  ) {
    super(message);
    this.name = "TabularLoadError";
  }
}

async function loadError(res: Response): Promise<TabularLoadError> {
  if (res.status === 404 || res.status === 403) {
    return new TabularLoadError("CSV が見つかりません。削除されたか、見る権限がありません。", true, false);
  }
  const invalid = await invalidInputMessage(res);
  if (invalid) return new TabularLoadError(invalid, false, true);
  return new TabularLoadError("サーバーで問題が起きました。続くときは管理者に連絡してください。", false, false);
}

export async function getSchema(nodeId: string): Promise<SchemaResponse> {
  const res = await apiFetch(`/files/${nodeId}/tabular/schema`);
  if (!res.ok) throw await loadError(res);
  return (await res.json()) as SchemaResponse;
}

export async function getRows(nodeId: string, offset: number): Promise<TableResponse> {
  const res = await apiFetch(`/files/${nodeId}/tabular/rows?offset=${offset}`);
  if (!res.ok) throw await loadError(res);
  return (await res.json()) as TableResponse;
}

export async function runQuery(nodeId: string, sql: string): Promise<TableResponse> {
  const res = await apiFetch(`/files/${nodeId}/tabular/query`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ sql }),
  });
  if (res.status === 400) {
    // 読めないファイル（文字コードを決められない CSV・読めない Excel など）は、サーバーの理由と直し方を出す。
    const invalid = await invalidInputMessage(res);
    if (invalid) throw new Error(invalid);
    const body = (await res.json().catch(() => null)) as { title?: string } | null;
    throw new Error(body?.title ?? "SQL が拒否されました（読み取り専用の SELECT のみ）");
  }
  if (!res.ok) throw new Error(`クエリに失敗しました (${res.status})`);
  return (await res.json()) as TableResponse;
}

export async function applyPatch(
  nodeId: string,
  baseRev: number,
  ops: PatchOp[],
): Promise<PatchResponse> {
  const res = await apiFetch(`/files/${nodeId}/tabular/patch`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ base_rev: baseRev, ops }),
  });
  if (res.status === 409) {
    const body = (await res.json().catch(() => null)) as
      | { base_rev?: number; current_rev?: number }
      | null;
    throw new TabularConflict(body?.base_rev ?? baseRev, body?.current_rev ?? baseRev);
  }
  if (!res.ok) {
    const invalid = await invalidInputMessage(res);
    if (invalid) throw new Error(invalid);
    const body = (await res.json().catch(() => null)) as { title?: string } | null;
    throw new Error(body?.title ?? `保存に失敗しました (${res.status})`);
  }
  return (await res.json()) as PatchResponse;
}

/// 新規保存で、保存先に同じ名前のファイルがある（409 の `name_taken`・BUG-150）。
///
/// - `fileName`: サーバーが保存しようとした名前（`.csv` を付けた名前）。
/// - `numbered`: 番号を付けて保存を頼んだのに、` (50)` まで埋まっていた。
export class CsvNameTaken extends Error {
  constructor(
    public readonly fileName: string,
    public readonly numbered: boolean,
  ) {
    super(
      numbered
        ? `保存先に「${fileName.replace(/\.csv$/i, "")}」の CSV が多すぎるため、番号を付けて保存できませんでした。名前を変えてから、もう一度保存してください。`
        : `保存先に「${fileName}」がすでにあります。名前を変えるか、番号を付けて保存してください。`,
    );
    this.name = "CsvNameTaken";
  }
}

/// サーバーが付ける拡張子（`.csv` で終わらない名前には付ける・tabular の save_new と同じ）。
export function csvFileName(name: string): string {
  return /\.csv$/i.test(name) ? name : `${name}.csv`;
}

/// 新規保存で、保存先のフォルダーに保存できない（書き込む権限が無い・削除されたか見られない・GAP-66）。
///
/// 呼び出し側はドライブのトップに保存し直せる（SQL の結果の「新規 CSV として保存」）。文は利用者向け。
export class CsvFolderUnavailable extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CsvFolderUnavailable";
  }
}

export async function saveNewCsv(input: {
  parentId?: string | null;
  name: string;
  csv: string;
  /// 省略は失敗にする（同じ名前があれば 409 の `name_taken` → [`CsvNameTaken`]）。
  onNameTaken?: OnNameTaken;
}): Promise<SaveResponse> {
  const res = await apiFetch(`/tabular/save`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: input.parentId ?? null,
      name: input.name,
      csv: input.csv,
      on_name_taken: input.onNameTaken,
    }),
  });
  if (res.ok) return (await res.json()) as SaveResponse;
  if (res.status === 409) {
    const body = (await res.json().catch(() => null)) as { code?: ConflictCode } | null;
    if (body?.code === "name_taken") {
      throw new CsvNameTaken(csvFileName(input.name.trim()), input.onNameTaken === "add_number");
    }
    throw new Error("いまの状態では保存できませんでした。少し待ってから、もう一度保存してください。");
  }
  const invalid = await invalidInputMessage(res);
  if (invalid) throw new Error(invalid);
  if (res.status === 403) {
    throw new CsvFolderUnavailable("保存先のフォルダーに保存する権限がありません。必要なら、フォルダーの持ち主に頼んでください。");
  }
  if (res.status === 404) {
    throw new CsvFolderUnavailable("保存先のフォルダーが見つかりません。削除されたか、見る権限がありません。");
  }
  if (res.status === 400) {
    throw new Error("内容に誤りがあるため、受け付けられませんでした。名前を見直してください。");
  }
  throw new Error("サーバーで問題が起きました。続くときは管理者に連絡してください。");
}

/// TableResponse を CSV 文字列へ（SQL 結果の「新規保存」用・RFC4180 準拠のクォート）。
export function tableToCsv(table: TableResponse): string {
  const esc = (s: string | null) => {
    const v = s ?? "";
    return /[",\n\r]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
  };
  const lines = [table.columns.map(esc).join(",")];
  for (const row of table.rows) lines.push(row.map(esc).join(","));
  return lines.join("\n") + "\n";
}
