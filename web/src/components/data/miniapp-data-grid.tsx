"use client";

/// ミニアプリの画面の ```data_grid ブロック（Task 15.7 の束縛・描画は Task 15.5）。
///
/// 中身の 1 行目がミニアプリの束ねた保存ビューの別名。行は `POST /mini-apps/{id}/ui-actions`
/// （`action_id = data_grid:{別名}`）でページごとに取り、画面からはページ位置と件数しか送らない
/// （条件・並べ替えはピンした版のビュー）。行・列は**閲覧者本人の権限**で評価される。列の型・表示名は
/// テーブルのスキーマ（閲覧者本人の viewer）から、列の順・見出し・幅・非表示はビューの表示設定から決める。

import { AlertTriangle, Table2 } from "lucide-react";
import * as React from "react";

import { DataGrid, type DataGridStatus } from "@/components/data/data-grid";
import { DataGridHostContext } from "@/components/data/data-grid-host";
import {
  getTable,
  getViewerContext,
  type DataTable,
  type DataViewRun,
  type QueryResult,
} from "@/lib/data-api";
import { resolveColumns } from "@/lib/data-view";

const EMPTY_SORT = new Map<string, "asc" | "desc">();

export function MiniAppDataGrid({ alias }: { alias: string }) {
  const host = React.useContext(DataGridHostContext);
  const [first, setFirst] = React.useState<DataViewRun | null>(null);
  const [table, setTable] = React.useState<DataTable | null>(null);
  const [masked, setMasked] = React.useState<ReadonlySet<string>>(new Set());
  const [error, setError] = React.useState<string | null>(null);
  const [status, setStatus] = React.useState<DataGridStatus | null>(null);

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
      })
      .catch((e) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, [host, alias]);

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
  return (
    <figure className="my-4 overflow-hidden rounded-xl border bg-card" data-testid={`miniapp-data-grid-${alias}`}>
      <figcaption className="flex items-center gap-2 border-b px-3 py-2 text-xs text-muted-foreground">
        <Table2 className="size-3.5" aria-hidden />
        <span className="font-medium text-foreground">{table?.name ?? alias}</span>
        <span className="ml-auto tabular-nums">
          {status ? `${status.loaded.toLocaleString("ja-JP")}${status.done ? "" : "+"} 件` : ""}
        </span>
      </figcaption>
      {/* 行が少ない表は行の分だけの高さにする（空行で縦に間延びさせない・最大 22rem）。 */}
      <div
        style={{
          height:
            status?.done && status.loaded < 9 ? `${36 + Math.max(status.loaded, 1) * 34 + 2}px` : "22rem",
        }}
      >
        {table && first ? (
          <DataGrid
            tableId={first.table_id}
            columns={columns}
            query={{}}
            grid={grid}
            canEdit={false}
            sortState={EMPTY_SORT}
            fetchPage={fetchPage}
            onStatus={setStatus}
            onQueryError={(e) => setError(e.message)}
            bare
          />
        ) : (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground" role="status">
            読み込み中…
          </div>
        )}
      </div>
    </figure>
  );
}
