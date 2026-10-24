/// 構造化データ（業務データのテーブル）の API クライアント（Task 15.5）。
///
/// 型はすべて OpenAPI（utoipa）から生成した `@/generated/api` を使う（手書きの型を作らない）。
/// 誤りは {@link DataApiError} に状態コードと構造化した理由（`code` / `message` / 索引の進捗など）を
/// 載せて投げ、画面が「再読込」「準備中」「承認が要る」を出し分けられるようにする。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type S = components["schemas"];

export type DataTable = S["DataTable"];
export type TableSchema = S["TableSchema"];
export type FieldDef = S["FieldDef"];
export type FieldType = S["FieldType"];
export type DataRecord = S["DataRecord"];
export type DataQuery = S["DataQuery"];
export type Condition = S["Condition"];
export type CondOp = S["CondOp"];
export type SortKey = S["SortKey"];
export type QueryResult = S["QueryResult"];
export type CountResult = S["CountResult"];
export type ViewSpec = S["ViewSpec"];
export type GridView = S["GridView"];
export type ColumnConfig = S["ColumnConfig"];
export type GroupConfig = S["GroupConfig"];
export type ConditionalFormat = S["ConditionalFormat"];
export type FormatColor = S["FormatColor"];
export type RowHeight = S["RowHeight"];
export type DataViewBody = S["DataViewBody"];
export type DataViewSummary = S["DataViewSummary"];
export type ViewResponse = S["ViewResponse"];
export type DataViewRun = S["DataViewRun"];
export type TableViewerContext = S["TableViewerContext"];
export type TableIndexOverview = S["TableIndexOverview"];
export type IndexPromotion = S["IndexPromotion"];
export type IndexStatus = S["IndexStatus"];
export type QueryRejection = S["QueryRejection"];
export type RecordUpsertedEvent = S["RecordUpsertedEvent"];
export type RecordRemovedEvent = S["RecordRemovedEvent"];
export type StreamReadyEvent = S["StreamReadyEvent"];
export type StreamResetEvent = S["StreamResetEvent"];
export type SchemaChangedEvent = S["SchemaChangedEvent"];
export type StreamResetReason = S["StreamResetReason"];
export type AwarenessEvent = S["AwarenessEvent"];
export type AwarenessRequest = S["AwarenessRequest"];
export type DataQueryRequest = S["DataQueryRequest"];
export type BulkImportStatus = S["BulkImportStatus"];
export type BulkIndexMode = S["BulkIndexMode"];
export type BulkRowError = S["BulkRowError"];
export type StartImportRequest = S["StartImportRequest"];
export type StartImportResponse = S["StartImportResponse"];
export type ImportErrorsResponse = S["ImportErrorsResponse"];

/// API の誤り（状態コード＋構造化した本文）。
export class DataApiError extends Error {
  constructor(
    public status: number,
    public body: Record<string, unknown> | null,
    message: string,
  ) {
    super(message);
    this.name = "DataApiError";
  }

  /// 構造化した誤りの種類（409: `rev_conflict` / `unique_violation` / `derived_recomputing` /
  /// `conflict` / `index_preparing` / `index_approval_required`、400: `import_invalid`、
  /// 413: `import_too_large` など）。本文に `code` が無ければ `null`。
  get code(): string | null {
    const c = this.body?.code;
    return typeof c === "string" ? c : null;
  }

  /// 索引の準備中（並べ替え・絞り込みに使えるまで待つ）。
  get indexPreparing(): { field: string; progress: number | null } | null {
    if (this.status !== 409 || this.code !== "index_preparing") return null;
    return {
      field: String(this.body?.field ?? ""),
      progress: typeof this.body?.progress === "number" ? (this.body.progress as number) : null,
    };
  }

  /// 索引列の上限を超える（owner の承認が要る）。
  get approvalRequired(): { field: string; indexed: number; cap: number } | null {
    if (this.status !== 409 || this.code !== "index_approval_required") return null;
    return {
      field: String(this.body?.field ?? ""),
      indexed: Number(this.body?.indexed_fields ?? 0),
      cap: Number(this.body?.max_indexed_fields ?? 0),
    };
  }

  /// rev の楽観ロックの衝突（他の人が先に更新した・`code=rev_conflict`）。
  get revConflict(): boolean {
    return this.status === 409 && this.code === "rev_conflict";
  }

  /// unique 制約の違反（同じ値のレコードが既にある・`code=unique_violation`）。
  get uniqueViolation(): boolean {
    return this.status === 409 && this.code === "unique_violation";
  }

  /// 材料化した派生列が参照先の変更を受けて再計算中（`code=derived_recomputing`）。
  get derivedRecomputing(): { field: string } | null {
    if (this.status !== 409 || this.code !== "derived_recomputing") return null;
    return { field: String(this.body?.field ?? "") };
  }
}

/// 構造化した誤りの既定の文言（本文に `message` が無いとき）。列の参照名は呼び出し側が表示名に
/// 直して出し直せるよう、ここでは列名を含めない。
function codeMessage(body: Record<string, unknown> | null): string | null {
  switch (body?.code) {
    case "rev_conflict":
      return "他の人が先に更新しました。最新の内容を読み込み直してください";
    case "unique_violation":
      return "同じ値のレコードが既にあります（重複できない列です）";
    case "derived_recomputing":
      return "参照先の変更を受けて列を再計算しています。しばらくしてから再試行してください";
    case "conflict":
      return "他の操作と競合しました。もう一度お試しください";
    case "index_preparing":
      return "この列の索引を準備しています。完了するまでお待ちください";
    case "index_approval_required":
      return "索引列の上限を超えるため、テーブルの管理者の承認が要ります";
    case "import_too_large": {
      const max = Number(body?.max_bytes ?? 0);
      return max > 0
        ? `ファイルが大きすぎます（上限 ${Math.floor(max / (1024 * 1024))} MB）。分割して取り込んでください`
        : "ファイルが大きすぎます。分割して取り込んでください";
    }
    default:
      return null;
  }
}

/// 状態コードごとの既定の文言（本文に理由が無いとき）。
function fallbackMessage(status: number): string {
  switch (status) {
    case 400:
      return "入力が正しくありません";
    case 401:
      return "ログインが切れました。もう一度ログインしてください";
    case 403:
      return "この操作の権限がありません";
    case 404:
      return "見つかりませんでした（削除されたか、権限がありません）";
    case 409:
      return "他の操作と競合しました。最新の内容を読み込み直してください";
    case 410:
      return "読み取りの基準が古くなりました。最初から読み込み直してください";
    case 429:
      return "混み合っています。しばらくしてから再試行してください";
    default:
      return `サーバーエラー（${status}）`;
  }
}

async function ok<T>(res: Response): Promise<T> {
  if (res.ok) {
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }
  let body: Record<string, unknown> | null = null;
  try {
    body = (await res.json()) as Record<string, unknown>;
  } catch {
    body = null;
  }
  const message =
    (typeof body?.message === "string" && body.message) ||
    (typeof body?.detail === "string" && body.detail) ||
    codeMessage(body) ||
    fallbackMessage(res.status);
  throw new DataApiError(res.status, body, message);
}

function send(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  };
}

const enc = encodeURIComponent;

// ---------------------------------------------------------------------------
// テーブル
// ---------------------------------------------------------------------------

export async function listTables(): Promise<DataTable[]> {
  const r = await ok<S["TableListResponse"]>(await apiFetch("/data/tables"));
  return r.items;
}

export async function getTable(id: string): Promise<DataTable> {
  return ok<DataTable>(await apiFetch(`/data/tables/${enc(id)}`));
}

export async function createTable(name: string, schema: TableSchema): Promise<DataTable> {
  return ok<DataTable>(await apiFetch("/data/tables", send("POST", { name, schema })));
}

export async function updateSchema(
  id: string,
  schema: TableSchema,
  expectedSchemaVersion: number,
): Promise<DataTable> {
  return ok<DataTable>(
    await apiFetch(
      `/data/tables/${enc(id)}/schema`,
      send("PUT", { schema, expected_schema_version: expectedSchemaVersion }),
    ),
  );
}

export async function deleteTable(id: string): Promise<void> {
  await ok<void>(await apiFetch(`/data/tables/${enc(id)}`, { method: "DELETE" }));
}

export async function getViewerContext(id: string): Promise<TableViewerContext> {
  return ok<TableViewerContext>(await apiFetch(`/data/tables/${enc(id)}/viewer-context`));
}

// ---------------------------------------------------------------------------
// クエリ・レコード
// ---------------------------------------------------------------------------

/// 宣言的クエリ（行述語・マスクは閲覧者本人の権限でサーバが合成する）。
///
/// `promoteIndexes` は未索引の列での並べ替え・絞り込みを**索引の自動昇格の契機にする**明示の要求
/// （data-platform §10）。**/data のグリッドで利用者が並べ替え・絞り込みを操作したときだけ** true にする
/// （それ以外の経路では未索引の列は 400 のまま）。
export async function runQuery(
  tableId: string,
  query: DataQuery,
  init?: { signal?: AbortSignal; promoteIndexes?: boolean },
): Promise<QueryResult> {
  const body: DataQueryRequest = init?.promoteIndexes ? { ...query, promote_indexes: true } : query;
  return ok<QueryResult>(
    await apiFetch(`/data/tables/${enc(tableId)}/query`, {
      ...send("POST", body),
      signal: init?.signal,
    }),
  );
}

export async function createRecord(tableId: string, data: Record<string, unknown>): Promise<DataRecord> {
  return ok<DataRecord>(await apiFetch(`/data/tables/${enc(tableId)}/records`, send("POST", { data })));
}

/// レコードを部分更新する（`expectedRev` の楽観ロック・不一致は `DataApiError.revConflict`）。
export async function updateRecord(
  tableId: string,
  recordId: string,
  patch: Record<string, unknown>,
  expectedRev: number,
): Promise<DataRecord> {
  return ok<DataRecord>(
    await apiFetch(
      `/data/tables/${enc(tableId)}/records/${enc(recordId)}`,
      send("PATCH", { patch, expected_rev: expectedRev }),
    ),
  );
}

export async function getRecord(tableId: string, recordId: string): Promise<DataRecord> {
  return ok<DataRecord>(await apiFetch(`/data/tables/${enc(tableId)}/records/${enc(recordId)}`));
}

export async function deleteRecord(tableId: string, recordId: string, expectedRev: number): Promise<void> {
  await ok<void>(
    await apiFetch(
      `/data/tables/${enc(tableId)}/records/${enc(recordId)}?expected_rev=${expectedRev}`,
      { method: "DELETE" },
    ),
  );
}

// ---------------------------------------------------------------------------
// 索引
// ---------------------------------------------------------------------------

export async function listIndexes(tableId: string): Promise<TableIndexOverview> {
  return ok<TableIndexOverview>(await apiFetch(`/data/tables/${enc(tableId)}/indexes`));
}

/// 列の索引を有効にする（2 万行未満は即時・それ以上は準備中）。上限超過は `approve` が要る。
export async function promoteIndex(tableId: string, field: string, approve = false): Promise<IndexPromotion> {
  return ok<IndexPromotion>(
    await apiFetch(`/data/tables/${enc(tableId)}/indexes`, send("POST", { field, approve })),
  );
}

// ---------------------------------------------------------------------------
// 保存ビュー
// ---------------------------------------------------------------------------

export async function listViews(tableId: string): Promise<DataViewSummary[]> {
  const r = await ok<S["ViewListResponse"]>(await apiFetch(`/data/tables/${enc(tableId)}/views`));
  return r.items;
}

export async function getView(id: string, version?: number): Promise<ViewResponse> {
  const q = version != null ? `?version=${version}` : "";
  return ok<ViewResponse>(await apiFetch(`/data/views/${enc(id)}${q}`));
}

export async function createView(name: string, body: DataViewBody): Promise<ViewResponse> {
  return ok<ViewResponse>(await apiFetch("/data/views", send("POST", { name, ...body })));
}

export async function updateView(
  id: string,
  body: DataViewBody,
  expectedVersion: number,
): Promise<ViewResponse> {
  return ok<ViewResponse>(
    await apiFetch(`/data/views/${enc(id)}`, send("PUT", { ...body, expected_version: expectedVersion })),
  );
}

// ---------------------------------------------------------------------------
// 変更の購読（SSE・data-platform §9）
// ---------------------------------------------------------------------------

export type RecordStreamHandlers = {
  onReady?: (position: number) => void;
  onUpserted: (e: RecordUpsertedEvent) => void;
  onRemoved: (e: RecordRemovedEvent) => void;
  /// 手元を捨てて読み直す（保持期間外・権限の変更・溢れ・スキーマ改訂）。接続はサーバが閉じる。
  /// `authz_changed` / `table_deleted` は**直前の行を見せ続けてはいけない**（呼び出し側が即時に消す）。
  onReset: (reason: StreamResetReason | "schema_changed") => void;
  /// 他ユーザーのカーソル位置（データを含まない）。
  onAwareness?: (e: AwarenessEvent) => void;
  /// 接続が切れた（ブラウザが Last-Event-ID 付きで自動で繋ぎ直す）。
  onError?: () => void;
  /// 繋ぎ直せない（403・404・401 などで EventSource が諦めた、または再接続の失敗が続いた）。購読は
  /// 閉じている。原因は EventSource から読めないので、呼び出し側が REST で確かめて案内する。
  onTerminal?: () => void;
};

/// 再接続の失敗がこの回数続いたら諦める（サーバが落ち続けている間に繋ぎ直しを繰り返さない）。
const MAX_STREAM_RETRIES = 6;

/// レコード変更を購読する。`since` は REST の応答の `snapshot_seq`（読取と購読の間の変更を取りこぼさない）。
///
/// 到達は at-least-once。冪等な適用（`(record_id, rev)`）は呼び出し側の責務。`stream.reset` /
/// `schema.changed` を受けたら購読を閉じて `onReset` を呼ぶ（以後の再接続は呼び出し側が読み直してから行う）。
export function subscribeRecords(
  tableId: string,
  since: number | null | undefined,
  h: RecordStreamHandlers,
): () => void {
  const q = since != null ? `?since=${since}` : "";
  const es = new EventSource(`/api/data/tables/${enc(tableId)}/stream${q}`, { withCredentials: true });
  let closed = false;
  let failures = 0;
  const close = () => {
    closed = true;
    es.close();
  };
  const parse = <T,>(ev: MessageEvent): T | null => {
    try {
      return JSON.parse(String(ev.data)) as T;
    } catch {
      return null;
    }
  };
  es.onopen = () => {
    failures = 0;
  };
  es.addEventListener("stream.ready", (ev) => {
    const d = parse<StreamReadyEvent>(ev as MessageEvent);
    if (d) h.onReady?.(d.position);
  });
  es.addEventListener("record.upserted", (ev) => {
    const d = parse<RecordUpsertedEvent>(ev as MessageEvent);
    if (d) h.onUpserted(d);
  });
  es.addEventListener("record.removed", (ev) => {
    const d = parse<RecordRemovedEvent>(ev as MessageEvent);
    if (d) h.onRemoved(d);
  });
  es.addEventListener("awareness", (ev) => {
    const d = parse<AwarenessEvent>(ev as MessageEvent);
    if (d) h.onAwareness?.(d);
  });
  es.addEventListener("stream.reset", (ev) => {
    const d = parse<StreamResetEvent>(ev as MessageEvent);
    close();
    h.onReset(d?.reason ?? "refresh");
  });
  es.addEventListener("schema.changed", () => {
    close();
    h.onReset("schema_changed");
  });
  es.onerror = () => {
    if (closed) return;
    // HTTP の誤り（403・404・401）や text/event-stream 以外の応答では、ブラウザは繋ぎ直さずに閉じる。
    failures += 1;
    if (es.readyState === EventSource.CLOSED || failures > MAX_STREAM_RETRIES) {
      close();
      h.onTerminal?.();
      return;
    }
    h.onError?.();
  };
  return close;
}

/// 自分のカーソル位置を他の閲覧者へ知らせる（データを含まない・`record_id: null` は離脱）。
export async function postAwareness(tableId: string, body: AwarenessRequest): Promise<void> {
  await ok<void>(await apiFetch(`/data/tables/${enc(tableId)}/awareness`, send("POST", body)));
}

// ---------------------------------------------------------------------------
// 一括取込（CSV・JSON の行・進捗・行エラー）
// ---------------------------------------------------------------------------

/// 取込の本文の上限（サーバの `MAX_IMPORT_BODY_BYTES` と同じ・超えると 413）。画面で先に知らせるための値で、
/// 正はサーバの検査。
export const MAX_IMPORT_BYTES = 32 * 1024 * 1024;

export type ImportOptions = { indexMode?: BulkIndexMode; abortOnInvalid?: boolean };

/// CSV で取込を始める（editor・見出し行の列はフィールドの参照名か表示名・`_key` は行キー）。
export async function startCsvImport(
  tableId: string,
  csv: Blob | string,
  opts: ImportOptions = {},
): Promise<StartImportResponse> {
  const q = new URLSearchParams();
  if (opts.indexMode) q.set("index_mode", opts.indexMode);
  if (opts.abortOnInvalid) q.set("abort_on_invalid", "true");
  const qs = q.toString();
  return ok<StartImportResponse>(
    await apiFetch(`/data/tables/${enc(tableId)}/imports/csv${qs ? `?${qs}` : ""}`, {
      method: "POST",
      headers: { "Content-Type": "text/csv; charset=utf-8" },
      body: csv,
    }),
  );
}

/// JSON の行で取込を始める。
export async function startJsonImport(tableId: string, body: StartImportRequest): Promise<StartImportResponse> {
  return ok<StartImportResponse>(await apiFetch(`/data/tables/${enc(tableId)}/imports`, send("POST", body)));
}

export async function getImport(tableId: string, importId: string): Promise<BulkImportStatus> {
  return ok<BulkImportStatus>(await apiFetch(`/data/tables/${enc(tableId)}/imports/${enc(importId)}`));
}

/// 不合格・拒否された行（行番号の昇順・`afterRow` は前ページの `next_after_row`）。
export async function listImportErrors(
  tableId: string,
  importId: string,
  afterRow?: number | null,
  limit = 100,
): Promise<ImportErrorsResponse> {
  const q = new URLSearchParams({ limit: String(limit) });
  if (afterRow != null) q.set("after_row", String(afterRow));
  return ok<ImportErrorsResponse>(
    await apiFetch(`/data/tables/${enc(tableId)}/imports/${enc(importId)}/errors?${q.toString()}`),
  );
}

// ---------------------------------------------------------------------------
// ミニアプリの画面の表（```data_grid・Task 15.7 の束縛）
// ---------------------------------------------------------------------------

/// ミニアプリが束ねた保存ビュー（別名）を 1 ページ実行する。送れるのはページ位置と件数だけで、条件・
/// 並べ替えはピンした版のビューの定義が使われる。行・列は閲覧者本人の権限で評価される。
export async function runMiniAppDataGrid(
  appId: string,
  version: number,
  alias: string,
  page: S["DataViewPage"],
): Promise<DataViewRun> {
  const r = await ok<S["UiActionResponse"]>(
    await apiFetch(
      `/mini-apps/${enc(appId)}/ui-actions`,
      send("POST", { version, action_id: `data_grid:${alias}`, params: page }),
    ),
  );
  // 結果は束縛種別ごとの判別共用体（生成型 `UiActionResult`）。表のボタンは `data_view` だけ。
  if (r.result.kind !== "data_view") {
    throw new DataApiError(500, null, "表の結果を読めませんでした");
  }
  return r.result.result;
}
