"use client";

/// 構造化データの仮想化グリッド（Task 15.5・glide-data-grid）。
///
/// - **keyset カーソルで無限スクロール**: 先頭ページを取ってすぐ描き、見えている範囲が読み込み済みの
///   末尾に近づいたら `next_cursor` で次のページを取る（OFFSET を使わないので何ページ目でも同じコスト）。
///   行は `csv-grid.tsx` と同じく canvas で仮想化して描く（DOM は見えている分だけ）。
/// - **セル編集は `rev` の楽観ロック**: 書いた値を先に見せ、409（他の人が先に更新）はその行を読み直す。
/// - **変更の購読（SSE）**: 先頭ページの `snapshot_seq` から購読し、`(record_id, rev)` で冪等に適用する
///   （手元の rev 以下・派生値だけの再配信で rev 未満は捨てる）。手元に無い行の出現は件数だけ数えて
///   「読み込む」を出す（並び順の途中に勝手に差し込まない）。`stream.reset` は手元を捨てて読み直す。
/// - 行グループ化は並べ替えの先頭キーで区切った見出し行、条件付き書式は表示中の値で評価する。

import "@glideapps/glide-data-grid/dist/index.css";

import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import { Loader2, RefreshCw } from "lucide-react";
import * as React from "react";

import { resolveColor, useGlideTheme } from "@/components/csv/glide-theme";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";
import {
  DataApiError,
  getRecord,
  runQuery,
  subscribeRecords,
  updateRecord,
  type CountResult,
  type DataQuery,
  type DataRecord,
  type GridView,
  type QueryResult,
} from "@/lib/data-api";
import {
  cellValue,
  FORMAT_COLORS,
  formatFor,
  formatValue,
  isEditable,
  parseInput,
  type ColumnModel,
} from "@/lib/data-view";

/// 先頭ページは小さく（初回描画を速く）、以降は上限いっぱいで取る。
const FIRST_PAGE = 100;
const NEXT_PAGE = 200;
/// 読み込み済みの末尾からこの行数以内が見えたら次のページを取る。
const PREFETCH_ROWS = 120;
/// 続きがあるときに末尾に置く「読み込み中」の行数。
const LOADING_ROWS = 20;

const ROW_HEIGHTS = { short: 34, medium: 56, tall: 88 } as const;

type DisplayRow =
  | { kind: "group"; level: number; key: string; label: string; count: number }
  | { kind: "row"; index: number };

export type DataGridHandle = {
  /// 手元を捨てて先頭から読み直す。
  reload: () => void;
};

export type DataGridStatus = {
  loaded: number;
  done: boolean;
  count: CountResult | null;
  firstRenderMs: number | null;
};

type Props = {
  tableId: string;
  /// 表示する列（非表示を除いた表示順）。
  columns: ColumnModel[];
  /// 実行するクエリ（`cursor` と `limit` はグリッドが決める）。
  query: DataQuery;
  grid: GridView | null;
  canEdit: boolean;
  /// 現在の並べ替え（見出しの矢印）。
  sortState: Map<string, "asc" | "desc">;
  onHeaderClick?: (key: string) => void;
  onColumnResize?: (key: string, width: number, done: boolean) => void;
  onColumnMove?: (fromKey: string, toKey: string) => void;
  onHeaderMenu?: (key: string, bounds: Rectangle) => void;
  /// 読み込みの失敗（索引の準備中・承認待ちを含む）。画面側が案内を出す。
  onQueryError?: (e: DataApiError | Error) => void;
  onStatus?: (s: DataGridStatus) => void;
  /// ページの取り方の差し替え（ミニアプリの ```data_grid は保存ビューの束縛で取る）。指定すると
  /// 件数・変更の購読はしない（読み取り専用の埋め込み表）。
  fetchPage?: (cursor: string | null, limit: number) => Promise<QueryResult>;
  /// 枠線・角丸を付けない（埋め込み先が枠を持つとき）。
  bare?: boolean;
};

function groupLabel(v: unknown, col: ColumnModel | undefined): string {
  const s = formatValue(v, col?.field ?? null, col?.key);
  return s === "" ? "（空）" : s;
}

export const DataGrid = React.forwardRef<DataGridHandle, Props>(function DataGrid(
  {
    tableId,
    columns,
    query,
    grid,
    canEdit,
    sortState,
    onHeaderClick,
    onColumnResize,
    onColumnMove,
    onHeaderMenu,
    onQueryError,
    onStatus,
    fetchPage,
    bare,
  },
  ref,
) {
  const fetchPageRef = React.useRef(fetchPage);
  fetchPageRef.current = fetchPage;
  const embedded = Boolean(fetchPage);
  const { theme: glideTheme, editedBg } = useGlideTheme();
  const rows = React.useRef<DataRecord[]>([]);
  const indexById = React.useRef<Map<string, number>>(new Map());
  const cursor = React.useRef<string | null>(null);
  const done = React.useRef(false);
  const loading = React.useRef(false);
  const generation = React.useRef(0);
  const pending = React.useRef<Set<string>>(new Set());
  const [version, setVersion] = React.useState(0);
  const bump = React.useCallback(() => setVersion((v) => v + 1), []);
  const [initial, setInitial] = React.useState<"loading" | "ready" | "error" | "preparing">("loading");
  const [newRows, setNewRows] = React.useState(0);
  const [collapsed, setCollapsed] = React.useState<Set<string>>(new Set());
  const [selection, setSelection] = React.useState<GridSelection>({
    columns: CompactSelection.empty(),
    rows: CompactSelection.empty(),
  });
  const status = React.useRef<DataGridStatus>({ loaded: 0, done: false, count: null, firstRenderMs: null });
  const startedAt = React.useRef(0);
  const onStatusRef = React.useRef(onStatus);
  onStatusRef.current = onStatus;
  const onErrorRef = React.useRef(onQueryError);
  onErrorRef.current = onQueryError;

  const queryKey = JSON.stringify(query);
  const groupKeys = React.useMemo(() => (grid?.group_by ?? []).map((g) => g.field), [grid]);

  const report = React.useCallback((patch: Partial<DataGridStatus>) => {
    status.current = { ...status.current, ...patch };
    onStatusRef.current?.(status.current);
  }, []);

  const rebuildIndex = () => {
    const m = new Map<string, number>();
    rows.current.forEach((r, i) => m.set(r.id, i));
    indexById.current = m;
  };

  const fetchNext = React.useCallback(async () => {
    if (loading.current || done.current) return;
    loading.current = true;
    const gen = generation.current;
    const first = rows.current.length === 0;
    try {
      const q: DataQuery = JSON.parse(queryKey) as DataQuery;
      const limit = first ? FIRST_PAGE : NEXT_PAGE;
      const res = fetchPageRef.current
        ? await fetchPageRef.current(cursor.current, limit)
        : await runQuery(tableId, { ...q, limit, cursor: cursor.current ?? undefined, with_count: false });
      if (gen !== generation.current) return;
      const items = res.items ?? [];
      for (const r of items) {
        if (indexById.current.has(r.id)) continue; // ライブページングの重複は捨てる
        indexById.current.set(r.id, rows.current.length);
        rows.current.push(r);
      }
      cursor.current = res.next_cursor ?? null;
      done.current = !res.next_cursor;
      if (first) {
        setInitial("ready");
        // 初回描画の所要（クエリ発行から先頭ページを描くまで）を計測に残す。
        requestAnimationFrame(() => {
          const ms = Math.round(performance.now() - startedAt.current);
          performance.mark("data-grid:first-render");
          report({ firstRenderMs: ms });
        });
        if (!fetchPageRef.current) {
          subscribe(res.snapshot_seq ?? null, gen);
          void fetchCount(gen);
        }
      }
      report({ loaded: rows.current.length, done: done.current });
      bump();
    } catch (e) {
      if (gen !== generation.current) return;
      // 索引の準備中・承認待ちは画面側の案内（進捗・承認）に任せ、ここは失敗として見せない。
      const waiting = e instanceof DataApiError && (e.indexPreparing || e.approvalRequired);
      if (first) setInitial(waiting ? "preparing" : "error");
      onErrorRef.current?.(e instanceof Error ? e : new Error(String(e)));
    } finally {
      if (gen === generation.current) loading.current = false;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tableId, queryKey, bump, report]);

  /// 件数は別リクエストで非同期に取る（ページの描画を待たせない・data-platform §5.4）。
  const fetchCount = async (gen: number) => {
    try {
      const q: DataQuery = JSON.parse(queryKey) as DataQuery;
      const res = await runQuery(tableId, { ...q, limit: 1, cursor: undefined, with_count: true, select: [] });
      if (gen === generation.current) report({ count: res.count ?? null });
    } catch {
      // 件数は付加情報（失敗しても行は見える）。
    }
  };

  const closeStream = React.useRef<(() => void) | null>(null);
  const subscribe = (since: number | null, gen: number) => {
    closeStream.current?.();
    closeStream.current = subscribeRecords(tableId, since, {
      onUpserted: ({ record, derived }) => {
        if (gen !== generation.current) return;
        const i = indexById.current.get(record.id);
        if (i === undefined) {
          setNewRows((n) => n + 1);
          return;
        }
        const cur = rows.current[i];
        // 冪等: 手元より新しい rev だけ（派生値だけの再配信は同じ rev でも適用）。
        if (derived ? record.rev < cur.rev : record.rev <= cur.rev) return;
        if (pending.current.has(record.id) && !derived) pending.current.delete(record.id);
        rows.current[i] = record;
        bump();
      },
      onRemoved: ({ record_id }) => {
        if (gen !== generation.current) return;
        if (!indexById.current.has(record_id)) return;
        rows.current = rows.current.filter((r) => r.id !== record_id);
        rebuildIndex();
        report({ loaded: rows.current.length });
        bump();
      },
      onReset: () => {
        if (gen !== generation.current) return;
        reset();
      },
    });
  };

  const reset = React.useCallback(() => {
    generation.current += 1;
    closeStream.current?.();
    closeStream.current = null;
    rows.current = [];
    indexById.current = new Map();
    cursor.current = null;
    done.current = false;
    loading.current = false;
    pending.current.clear();
    setNewRows(0);
    setInitial("loading");
    startedAt.current = performance.now();
    report({ loaded: 0, done: false, count: null, firstRenderMs: null });
    bump();
    void fetchNext();
  }, [bump, fetchNext, report]);

  React.useEffect(() => {
    reset();
    return () => {
      generation.current += 1;
      closeStream.current?.();
    };
  }, [reset]);

  React.useImperativeHandle(ref, () => ({ reload: reset }), [reset]);

  // ---- 表示行（グループ見出しを挟む） ----
  const colByKey = React.useMemo(() => new Map(columns.map((c) => [c.key, c])), [columns]);
  const display: DisplayRow[] = React.useMemo(() => {
    if (groupKeys.length === 0) return rows.current.map((_, index) => ({ kind: "row", index }));
    const out: DisplayRow[] = [];
    const counts = new Map<string, number>();
    const pathOf = (r: DataRecord) =>
      groupKeys.map((k) => JSON.stringify(cellValue(r, k) ?? null));
    for (const r of rows.current) {
      const p = pathOf(r);
      for (let l = 0; l < p.length; l++) {
        const key = p.slice(0, l + 1).join("\u0001");
        counts.set(key, (counts.get(key) ?? 0) + 1);
      }
    }
    let prev: string[] = [];
    rows.current.forEach((r, index) => {
      const p = pathOf(r);
      let hiddenBy = false;
      for (let l = 0; l < p.length; l++) {
        const key = p.slice(0, l + 1).join("\u0001");
        if (prev.slice(0, l + 1).join("\u0001") !== key) {
          // 祖先が畳まれていれば見出しも出さない。
          const ancestorCollapsed = p.slice(0, l).some((_, j) => collapsed.has(p.slice(0, j + 1).join("\u0001")));
          if (!ancestorCollapsed) {
            const col = colByKey.get(groupKeys[l]);
            out.push({
              kind: "group",
              level: l,
              key,
              label: `${col?.label ?? groupKeys[l]}: ${groupLabel(cellValue(r, groupKeys[l]), col)}`,
              count: counts.get(key) ?? 0,
            });
          }
        }
        if (collapsed.has(key)) hiddenBy = true;
      }
      prev = p;
      if (!hiddenBy) out.push({ kind: "row", index });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, groupKeys, collapsed, colByKey]);

  const loadingRows = done.current ? 0 : LOADING_ROWS;
  const totalRows = display.length + (initial === "ready" ? loadingRows : 0);

  const gridColumns: GridColumn[] = React.useMemo(
    () =>
      columns.map((c) => {
        const s = sortState.get(c.key);
        return {
          id: c.key,
          title: s ? `${c.label} ${s === "asc" ? "↑" : "↓"}` : c.label,
          width: c.width,
          hasMenu: Boolean(onHeaderMenu),
        };
      }),
    [columns, sortState, onHeaderMenu],
  );
  const freezeColumns = React.useMemo(() => {
    let n = 0;
    for (const c of columns) {
      if (!c.frozen) break;
      n += 1;
    }
    return n;
  }, [columns]);

  // 書式の色は glide が oklch を読めないので rgb に直して使う（テーマ切替で作り直す）。
  const formatTheme = React.useMemo(() => {
    const m = new Map<string, Partial<Theme>>();
    if (typeof document === "undefined") return m;
    for (const [k, v] of Object.entries(FORMAT_COLORS)) {
      const probe = document.createElement("div");
      probe.style.backgroundColor = v.bg;
      probe.style.color = v.fg;
      document.body.appendChild(probe);
      const cs = getComputedStyle(probe);
      m.set(k, { bgCell: resolveColor(cs.backgroundColor), textDark: resolveColor(cs.color) });
      probe.remove();
    }
    return m;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [glideTheme]);

  const getCellContent = React.useCallback(
    ([col, row]: Item): GridCell => {
      const d = display[row];
      if (!d) return { kind: GridCellKind.Loading, allowOverlay: false };
      if (d.kind === "group") {
        const open = !collapsed.has(d.key);
        return {
          kind: GridCellKind.Text,
          data: d.label,
          displayData: `${"　".repeat(d.level)}${open ? "▾" : "▸"} ${d.label}（${d.count}${done.current ? "" : "+"}）`,
          allowOverlay: false,
          readonly: true,
          span: [0, columns.length - 1],
          themeOverride: { bgCell: glideTheme.bgHeader, baseFontStyle: "600 13px" },
        };
      }
      const r = rows.current[d.index];
      const c = columns[col];
      if (!r || !c) return { kind: GridCellKind.Loading, allowOverlay: false };
      const v = cellValue(r, c.key);
      const text = formatValue(v, c.field, c.key);
      const editable = canEdit && isEditable(c.field);
      const fmt = formatFor(grid, r, c.key);
      const numeric = c.field?.type === "number" || c.field?.result_type === "number";
      const override: Partial<Theme> | undefined = pending.current.has(r.id)
        ? { bgCell: editedBg }
        : fmt
          ? { ...formatTheme.get(fmt.color), ...(fmt.bold ? { baseFontStyle: "600 13px" } : {}) }
          : undefined;
      return {
        kind: GridCellKind.Text,
        data: Array.isArray(v) ? v.join(", ") : v == null ? "" : String(v),
        displayData: text,
        allowOverlay: editable,
        readonly: !editable,
        contentAlign: numeric ? "right" : undefined,
        themeOverride: override,
      };
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [display, columns, canEdit, grid, collapsed, editedBg, formatTheme, glideTheme, version],
  );

  const onCellEdited = React.useCallback(
    ([col, row]: Item, cell: EditableGridCell) => {
      if (cell.kind !== GridCellKind.Text) return;
      const d = display[row];
      const c = columns[col];
      if (!d || d.kind !== "row" || !c?.field) return;
      const i = d.index;
      const before = rows.current[i];
      const parsed = parseInput(cell.data, c.field);
      if (!parsed.ok) {
        toast({ variant: "destructive", title: `「${c.label}」を保存できません`, description: parsed.error });
        return;
      }
      const prevValue = cellValue(before, c.key);
      if (JSON.stringify(prevValue ?? null) === JSON.stringify(parsed.value ?? null)) return;
      // 先に見せる（保存中は薄く色を付ける）。
      rows.current[i] = { ...before, data: { ...(before.data as object), [c.key]: parsed.value } };
      pending.current.add(before.id);
      bump();
      updateRecord(tableId, before.id, { [c.key]: parsed.value }, before.rev)
        .then((saved) => {
          const j = indexById.current.get(saved.id);
          if (j !== undefined && saved.rev >= rows.current[j].rev) rows.current[j] = saved;
        })
        .catch(async (e: unknown) => {
          const j = indexById.current.get(before.id);
          if (e instanceof DataApiError && e.revConflict) {
            // 他の人が先に更新した: 最新を読み直して見せる（自分の入力は反映しない）。
            try {
              const latest = await getRecord(tableId, before.id);
              if (j !== undefined) rows.current[j] = latest;
            } catch {
              if (j !== undefined) rows.current[j] = before;
            }
            toast({
              title: "他の人が先に更新しました",
              description: "最新の内容を読み込み直しました。必要ならもう一度入力してください。",
            });
          } else {
            if (j !== undefined) rows.current[j] = before;
            toast({
              variant: "destructive",
              title: "保存できませんでした",
              description: e instanceof Error ? e.message : String(e),
            });
          }
        })
        .finally(() => {
          pending.current.delete(before.id);
          bump();
        });
    },
    [display, columns, tableId, bump],
  );

  const onVisibleRegionChanged = React.useCallback(
    (r: Rectangle) => {
      if (r.y + r.height + PREFETCH_ROWS >= display.length) void fetchNext();
    },
    [display.length, fetchNext],
  );

  const onCellClicked = React.useCallback(
    ([, row]: Item) => {
      const d = display[row];
      if (d?.kind !== "group") return;
      setCollapsed((s) => {
        const n = new Set(s);
        if (n.has(d.key)) n.delete(d.key);
        else n.add(d.key);
        return n;
      });
    },
    [display],
  );

  const rowHeight = ROW_HEIGHTS[grid?.row_height ?? "short"];

  return (
    <div
      className={cn(
        "data-grid relative h-full w-full overflow-hidden bg-card",
        !bare && "rounded-lg border border-border shadow-xs",
      )}
      data-testid="data-grid"
      data-loaded={status.current.loaded}
      data-first-render-ms={status.current.firstRenderMs ?? undefined}
    >
      {initial === "loading" ? (
        <div
          className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-card/70 text-sm text-muted-foreground"
          role="status"
        >
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : null}
      {initial === "preparing" ? (
        <div className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-card text-sm text-muted-foreground" role="status">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          並べ替え・絞り込みの準備ができたら表示します
        </div>
      ) : null}
      {initial === "error" ? (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-card text-sm text-muted-foreground">
          <p>行を読み込めませんでした</p>
          <button
            type="button"
            onClick={reset}
            className="inline-flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium text-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <RefreshCw className="size-3.5" aria-hidden />
            もう一度読み込む
          </button>
        </div>
      ) : null}
      {initial === "ready" && display.length === 0 && done.current ? (
        <div className="pointer-events-none absolute inset-x-0 top-24 z-10 text-center text-sm text-muted-foreground">
          条件に合うレコードはありません
        </div>
      ) : null}
      {newRows > 0 ? (
        <button
          type="button"
          onClick={reset}
          data-testid="data-grid-new-rows"
          className="absolute bottom-4 left-1/2 z-20 inline-flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-primary/30 bg-card px-3 py-1.5 text-xs font-medium text-primary shadow-md transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <RefreshCw className="size-3.5" aria-hidden />
          新しいレコード {newRows} 件 · 表示する
        </button>
      ) : null}
      <DataEditor
        theme={glideTheme}
        columns={gridColumns}
        rows={totalRows}
        rowHeight={rowHeight}
        getCellContent={getCellContent}
        onCellEdited={canEdit ? onCellEdited : undefined}
        onCellClicked={groupKeys.length ? onCellClicked : undefined}
        onVisibleRegionChanged={onVisibleRegionChanged}
        gridSelection={selection}
        onGridSelectionChange={setSelection}
        onHeaderClicked={onHeaderClick ? (col) => columns[col] && onHeaderClick(columns[col].key) : undefined}
        onHeaderMenuClick={onHeaderMenu ? (col, b) => columns[col] && onHeaderMenu(columns[col].key, b) : undefined}
        onColumnResize={
          onColumnResize ? (c, w) => onColumnResize(String(c.id), w, false) : undefined
        }
        onColumnResizeEnd={
          onColumnResize ? (c, w) => onColumnResize(String(c.id), w, true) : undefined
        }
        onColumnMoved={
          onColumnMove
            ? (from, to) => {
                const a = columns[from];
                const b = columns[to];
                if (a && b) onColumnMove(a.key, b.key);
              }
            : undefined
        }
        freezeColumns={freezeColumns}
        // 見出しのクリックは並べ替え（列の選択にしない・選択色で見出しが読めなくなるのを避ける）。
        columnSelect="none"
        rowMarkers={embedded ? "none" : "number"}
        smoothScrollX
        smoothScrollY
        width="100%"
        height="100%"
        getCellsForSelection
      />
      <div id="portal" style={{ position: "fixed", left: 0, top: 0, zIndex: 9999 }} />
    </div>
  );
});

