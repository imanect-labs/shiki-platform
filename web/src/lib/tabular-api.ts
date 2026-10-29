/// CSV クエリ/パッチ API クライアント（Task 11P.7/11P.8）。型は OpenAPI 生成。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type SchemaResponse = components["schemas"]["SchemaResponse"];
export type TableResponse = components["schemas"]["TableResponse"];
export type PatchResponse = components["schemas"]["PatchResponse"];
export type SaveResponse = components["schemas"]["SaveResponse"];

/// パッチ操作（backend の tabular::PatchOp と 1:1・タグ付き union）。
export type PatchOp =
  | { op: "cell_update"; row: number; col: number; value: string }
  | { op: "row_insert"; at: number; values: string[] }
  | { op: "row_delete"; row: number }
  | { op: "column_add"; name: string }
  | { op: "column_delete"; col: number }
  | { op: "column_rename"; col: number; name: string };

/// 楽観ロック競合（409）。UI はリロードを促す。
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

export async function saveNewCsv(input: {
  parentId?: string | null;
  name: string;
  csv: string;
}): Promise<SaveResponse> {
  const res = await apiFetch(`/tabular/save`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ parent_id: input.parentId ?? null, name: input.name, csv: input.csv }),
  });
  // 同名ノードは 409（新規作成のため）。呼び出し側が連番リトライ等で扱えるよう区別する。
  if (res.status === 409) throw new TabularConflict(0, 0);
  if (!res.ok) throw new Error(`CSV 保存に失敗しました (${res.status})`);
  return (await res.json()) as SaveResponse;
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
