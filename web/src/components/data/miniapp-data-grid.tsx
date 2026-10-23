"use client";

/// ミニアプリの画面の ```data_grid ブロック（Task 15.7 の束縛・描画は Task 15.5）。
///
/// 中身の 1 行目がミニアプリの束ねた保存ビューの別名。行は `POST /mini-apps/{id}/ui-actions`
/// （`action_id = data_grid:{別名}`）でページごとに取り、画面からはページ位置と件数しか送らない
/// （条件・並べ替えはピンした版のビュー）。行・列は**閲覧者本人の権限**で評価される。列の型・表示名は
/// テーブルのスキーマ（閲覧者本人の viewer）から、列の順・見出し・幅・非表示はビューの表示設定から決める。

import { AlertTriangle, Inbox, Loader2, Table2 } from "lucide-react";
import * as React from "react";

import { DataGrid, type DataGridStatus } from "@/components/data/data-grid";
import { DataGridHostContext } from "@/components/data/data-grid-host";
import {
  DataApiError,
  getTable,
  getViewerContext,
  type DataTable,
  type DataViewRun,
  type QueryResult,
} from "@/lib/data-api";
import { resolveColumns } from "@/lib/data-view";

const EMPTY_SORT = new Map<string, "asc" | "desc">();

/// 埋め込み表の寸法（data-grid.tsx の既定と同じ）。行が少なければ行の分だけの高さにし、多ければ上限で内側を
/// スクロールさせる（空の罫線行で縦に間延びさせない）。
const HEADER_PX = 36;
const ROW_PX = 34;
const MAX_HEIGHT_PX = 352;
/// 索引の準備中に読み直す間隔。
const RETRY_MS = 3000;

function heightFor(rows: number, done: boolean): number {
  if (!done) return MAX_HEIGHT_PX;
  return Math.min(MAX_HEIGHT_PX, HEADER_PX + Math.max(rows, 1) * ROW_PX + 2);
}

type Waiting = { field: string; progress: number | null };

function waitingOf(e: unknown): Waiting | null {
  return e instanceof DataApiError ? (e.indexPreparing ?? null) : null;
}

export function MiniAppDataGrid({ alias }: { alias: string }) {
  const host = React.useContext(DataGridHostContext);
  const [first, setFirst] = React.useState<DataViewRun | null>(null);
  const [table, setTable] = React.useState<DataTable | null>(null);
  const [masked, setMasked] = React.useState<ReadonlySet<string>>(new Set());
  const [error, setError] = React.useState<string | null>(null);
  // 索引の準備中（失敗ではない待ち）。整うまで一定間隔で読み直す。
  const [waiting, setWaiting] = React.useState<Waiting | null>(null);
  const [attempt, setAttempt] = React.useState(0);
  const [status, setStatus] = React.useState<DataGridStatus | null>(null);

  const onFailure = React.useCallback((e: unknown) => {
    const w = waitingOf(e);
    if (w) setWaiting(w);
    else setError(e instanceof Error ? e.message : String(e));
  }, []);

  React.useEffect(() => {
    if (!host) return;
    let active = true;
    setError(null);
    // 先頭ページで表示設定と対象テーブルを知り、スキーマ（列の型・表示名）を閲覧者本人の権限で読む。
    host
      .run(alias, { limit: 1 })
      .then(async (run) => {
        const [t, v] = await Promise.all([getTable(run.table_id), getViewerContext(run.table_id)]);
        if (!active) return;
        setFirst(run);
        setTable(t);
        setMasked(new Set(v.masked_fields));
        setWaiting(null);
      })
      .catch((e) => active && onFailure(e));
    return () => {
      active = false;
    };
  }, [host, alias, attempt, onFailure]);

  React.useEffect(() => {
    if (!waiting) return;
    const t = window.setTimeout(() => setAttempt((n) => n + 1), RETRY_MS);
    return () => window.clearTimeout(t);
  }, [waiting, attempt]);

  const fetchPage = React.useCallback(
    async (cursor: string | null, limit: number): Promise<QueryResult> => {
      if (!host) throw new Error("この画面では表を描けません");
      const run = await host.run(alias, { ...(cursor ? { cursor } : {}), limit });
      return run.result;
    },
    [host, alias],
  );

  if (!host) {
    return (
      <p className="my-3 rounded-lg border border-dashed px-3 py-2 text-xs text-muted-foreground">
        表「{alias}」はミニアプリの画面でだけ表示できます
      </p>
    );
  }
  if (error) {
    return (
      <div className="my-3 flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-xs text-destructive" role="alert">
        <AlertTriangle className="size-4 shrink-0" aria-hidden />
        表「{alias}」を表示できません: {error}
      </div>
    );
  }
  const grid = first?.display.kind === "grid" ? first.display : null;
  const columns = table ? resolveColumns(table.schema.fields, grid, masked).filter((c) => !c.hidden) : [];
  const empty = first !== null && (first.result.items ?? []).length === 0 && !first.result.next_cursor;
  // 先頭ページの前に待ちになったときはスキーマが無いので、列名（参照名）は出さずに一般の文言にする。
  const label = waiting ? columns.find((c) => c.key === waiting.field)?.label : undefined;
  return (
    <figure className="my-4 overflow-hidden rounded-xl border bg-card" data-testid={`miniapp-data-grid-${alias}`}>
      <figcaption className="flex items-center gap-2 border-b px-3 py-2 text-xs text-muted-foreground">
        <Table2 className="size-3.5" aria-hidden />
        <span className="font-medium text-foreground">{table?.name ?? alias}</span>
        <span className="ml-auto tabular-nums">
          {empty ? "0 件" : status ? `${status.loaded.toLocaleString("ja-JP")}${status.done ? "" : "+"} 件` : ""}
        </span>
      </figcaption>
      {waiting ? (
        <div className="flex flex-col items-center justify-center gap-2 px-4 py-8 text-sm text-muted-foreground" role="status" data-testid="miniapp-data-grid-waiting">
          <span className="inline-flex items-center gap-2 text-foreground">
            <Loader2 className="size-4 animate-spin text-primary" aria-hidden />
            {label ? `「${label}」で並べ替え・絞り込みできるよう準備中です` : "表の並べ替え・絞り込みを準備中です"}
            {waiting.progress != null ? `（${Math.round(waiting.progress * 100)}%）` : ""}
          </span>
          <span className="text-xs">準備ができたら自動で表示します</span>
        </div>
      ) : empty ? (
        <div className="flex flex-col items-center justify-center gap-1.5 px-4 py-8 text-sm text-muted-foreground" data-testid="miniapp-data-grid-empty">
          <Inbox className="size-5" aria-hidden />
          表示するレコードはありません
        </div>
      ) : table && first ? (
        <div style={{ height: heightFor(status?.loaded ?? 0, status?.done ?? false) }}>
          <DataGrid
            key={attempt}
            tableId={first.table_id}
            columns={columns}
            query={{}}
            grid={grid}
            canEdit={false}
            sortState={EMPTY_SORT}
            fetchPage={fetchPage}
            onStatus={setStatus}
            onQueryError={onFailure}
            bare
          />
        </div>
      ) : (
        <div className="flex items-center justify-center gap-2 py-8 text-xs text-muted-foreground" role="status">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
          読み込み中…
        </div>
      )}
    </figure>
  );
}
