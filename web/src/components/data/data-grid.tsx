"use client";

/// 構造化データの仮想化グリッド（Task 15.5・glide-data-grid）。
///
/// - **keyset カーソルで無限スクロール**: 先頭ページを取ってすぐ描き、見えている範囲が読み込み済みの
///   末尾に近づいたら `next_cursor` で次のページを取る（OFFSET を使わないので何ページ目でも同じコスト）。
///   行は `csv-grid.tsx` と同じく canvas で仮想化して描く（DOM は見えている分だけ）。
/// - **セル編集は `rev` の楽観ロック**: 書いた値を先に見せ、409（他の人が先に更新）はその行を読み直す。
/// - **変更の購読（SSE）**: 先頭ページの `snapshot_seq` から購読し、`(record_id, rev)` で冪等に適用する
///   （手元の rev 以下・派生値だけの再配信で rev 未満は捨てる）。手元に無い行の出現は件数だけ数えて
///   「読み込む」を出す（並び順の途中に勝手に差し込まない）。`stream.reset` は手元を捨てて読み直す
///   （権限の変更 `authz_changed`・テーブルの削除は直前の行を薄く残さず**即時に消して**から読み直す）。
///   購読が 403 などで繋ぎ直せなくなったら止め、REST で原因を確かめて案内する（再接続の嵐にしない）。
/// - **awareness**: 選んでいるセルの位置（行 id・列の参照名だけ・値なし）を送り、他の人の位置を色付きの
///   枠で描く（data-platform §9.5）。
/// - 行グループ化は並べ替えの先頭キーで区切った見出し行、条件付き書式は表示中の値で評価する。

import "@glideapps/glide-data-grid/dist/index.css";

import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  type DataEditorRef,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import { Loader2, RefreshCw, WifiOff } from "lucide-react";
import * as React from "react";

import { resolveColor, useGlideTheme } from "@/components/csv/glide-theme";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";
import {
  AWARENESS_HEARTBEAT_MS,
  PEER_TTL_MS,
  peerColor,
  withAlpha,
  type Peer,
} from "@/lib/data-awareness";
import {
  DataApiError,
  getRecord,
  getViewerContext,
  postAwareness,
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
  /// 索引の準備中・承認待ちの案内（失敗ではない待ちの状態・整ったら呼び出し側が `reload` する）。
  waiting?: { message: string; progress: number | null } | null;
  /// 未索引の列での並べ替え・絞り込みを索引の自動昇格の契機にする（/data の画面で利用者が操作したときだけ・
  /// data-platform §10）。埋め込み（`fetchPage`）では使わない。
  promoteIndexes?: boolean;
  /// 他ユーザーの位置が変わった（ツールバーに顔ぶれを出すため）。
  onPeers?: (peers: Peer[]) => void;
};

/// 購読が止まった理由（グリッドの下に案内を出す）。
type LiveProblem = { kind: "disconnected" } | { kind: "signed_out" };

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
    waiting,
    promoteIndexes,
    onPeers,
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
  /// 読み直しの間（並べ替え・絞り込みの変更・索引の準備中）に薄く見せておく直前の行。
  /// 新しい先頭ページが届いたら捨てる。見せている間は編集させない（新しい条件の行ではないため）。
  const stale = React.useRef<DataRecord[]>([]);
  const [version, setVersion] = React.useState(0);
  const bump = React.useCallback(() => setVersion((v) => v + 1), []);
  const [initial, setInitial] = React.useState<"loading" | "ready" | "error" | "preparing">("loading");
  const [newRows, setNewRows] = React.useState(0);
  const [collapsed, setCollapsed] = React.useState<Set<string>>(new Set());
  const editorRef = React.useRef<DataEditorRef>(null);
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
  const promoteRef = React.useRef(promoteIndexes);
  promoteRef.current = promoteIndexes;
  const onPeersRef = React.useRef(onPeers);
  onPeersRef.current = onPeers;
  const [live, setLive] = React.useState<LiveProblem | null>(null);
  /// 読み込みの失敗の文言（権限を失ったときは既定の「読み込めませんでした」より具体的に出す）。
  const [failure, setFailure] = React.useState<string | null>(null);
  const peers = React.useRef<Map<string, Peer>>(new Map());
  const [peerVersion, setPeerVersion] = React.useState(0);
  const publishPeers = React.useCallback(() => {
    setPeerVersion((v) => v + 1);
    onPeersRef.current?.([...peers.current.values()]);
  }, []);

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
      // 昇格の要求は並べ替え・絞り込みがあるときだけ（無ければ索引は関係しない）。
      const promote = Boolean(promoteRef.current) && Boolean(q.filter || q.sort?.length);
      const res = fetchPageRef.current
        ? await fetchPageRef.current(cursor.current, limit)
        : await runQuery(
            tableId,
            { ...q, limit, cursor: cursor.current ?? undefined, with_count: false },
            { promoteIndexes: promote },
          );
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
        stale.current = [];
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
      if (first) {
        setFailure(
          e instanceof DataApiError && (e.status === 403 || e.status === 404)
            ? "このテーブルを見る権限がありません（共有が外されたか、削除されました）"
            : null,
        );
        setInitial(waiting ? "preparing" : "error");
      }
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
      onReset: (reason) => {
        if (gen !== generation.current) return;
        // 権限が変わった・テーブルが消えた: 見えなくなったかもしれない行を一瞬でも残さない。
        reset({ dropStale: reason === "authz_changed" || reason === "table_deleted" });
      },
      onAwareness: (a) => {
        if (gen !== generation.current) return;
        if (!a.record_id) peers.current.delete(a.principal);
        else peers.current.set(a.principal, { principal: a.principal, recordId: a.record_id, field: a.field ?? null, seenAt: Date.now() });
        publishPeers();
      },
      onError: () => {
        // ブラウザが繋ぎ直している（Last-Event-ID 付き）。案内は諦めたときだけ出す。
      },
      onTerminal: () => {
        if (gen !== generation.current) return;
        closeStream.current = null;
        void diagnoseStream(gen);
      },
    });
  };

  /// 購読が繋ぎ直せなくなった理由を REST で確かめる（EventSource は状態コードを読めない）。
  const diagnoseStream = async (gen: number) => {
    try {
      await getViewerContext(tableId);
      if (gen === generation.current) setLive({ kind: "disconnected" });
    } catch (e) {
      if (gen !== generation.current) return;
      if (e instanceof DataApiError && (e.status === 403 || e.status === 404)) {
        // 見る権限を失った: 手元の行を即時に捨てて案内だけにする。
        generation.current += 1;
        rows.current = [];
        stale.current = [];
        indexById.current = new Map();
        peers.current.clear();
        publishPeers();
        setFailure("このテーブルを見る権限がなくなりました（削除されたか、共有が外されました）");
        setInitial("error");
        report({ loaded: 0, done: false, count: null });
        bump();
      } else if (e instanceof DataApiError && e.status === 401) {
        setLive({ kind: "signed_out" });
      } else {
        setLive({ kind: "disconnected" });
      }
    }
  };

  const reset = React.useCallback((opts?: { dropStale?: boolean }) => {
    generation.current += 1;
    closeStream.current?.();
    closeStream.current = null;
    if (opts?.dropStale) stale.current = [];
    else if (rows.current.length) stale.current = rows.current;
    rows.current = [];
    indexById.current = new Map();
    cursor.current = null;
    done.current = false;
    loading.current = false;
    pending.current.clear();
    if (peers.current.size) {
      peers.current.clear();
      publishPeers();
    }
    setLive(null);
    setFailure(null);
    setNewRows(0);
    // 読み直しの前の選択は新しい行と対応しないので外す（薄く残す直前の行に枠を残さない）。
    setSelection({ columns: CompactSelection.empty(), rows: CompactSelection.empty() });
    setInitial("loading");
    startedAt.current = performance.now();
    report({ loaded: 0, done: false, count: null, firstRenderMs: null });
    bump();
    void fetchNext();
  }, [bump, fetchNext, report, publishPeers]);

  React.useEffect(() => {
    reset();
    return () => {
      generation.current += 1;
      closeStream.current?.();
    };
  }, [reset]);

  React.useImperativeHandle(ref, () => ({ reload: () => reset() }), [reset]);

  // ---- 表示行（グループ見出しを挟む） ----
  const colByKey = React.useMemo(() => new Map(columns.map((c) => [c.key, c])), [columns]);
  const showingStale = initial !== "ready" && rows.current.length === 0 && stale.current.length > 0;
  const source = showingStale ? stale.current : rows.current;
  const display: DisplayRow[] = React.useMemo(() => {
    if (groupKeys.length === 0) return source.map((_, index) => ({ kind: "row", index }));
    const out: DisplayRow[] = [];
    const counts = new Map<string, number>();
    const pathOf = (r: DataRecord) =>
      groupKeys.map((k) => JSON.stringify(cellValue(r, k) ?? null));
    for (const r of source) {
      const p = pathOf(r);
      for (let l = 0; l < p.length; l++) {
        const key = p.slice(0, l + 1).join("\u0001");
        counts.set(key, (counts.get(key) ?? 0) + 1);
      }
    }
    let prev: string[] = [];
    source.forEach((r, index) => {
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
  }, [version, groupKeys, collapsed, colByKey, showingStale]);

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
      const r = source[d.index];
      const c = columns[col];
      if (!r || !c) return { kind: GridCellKind.Loading, allowOverlay: false };
      const v = cellValue(r, c.key);
      const text = formatValue(v, c.field, c.key);
      const editable = canEdit && !showingStale && isEditable(c.field);
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
    [display, columns, canEdit, grid, collapsed, editedBg, formatTheme, glideTheme, version, source, showingStale],
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
            const recomputing = e instanceof DataApiError ? e.derivedRecomputing : null;
            const labelOf = (key: string) => columns.find((x) => x.key === key)?.label ?? key;
            toast({
              variant: "destructive",
              title:
                e instanceof DataApiError && e.uniqueViolation
                  ? `「${c.label}」に同じ値のレコードが既にあります`
                  : recomputing
                    ? `「${labelOf(recomputing.field)}」を再計算しています`
                    : "保存できませんでした",
              description:
                e instanceof DataApiError && e.uniqueViolation
                  ? "この列は重複できません。別の値を入れてください（入力は元に戻しました）。"
                  : recomputing
                    ? "参照先の変更を受けた再計算が終わるまで、この行は保存できません。しばらくしてから再試行してください。"
                    : e instanceof Error
                      ? e.message
                      : String(e),
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
      // glide はクリックしたセルを選んだあと、グリッドへのフォーカスを次の描画フレームまで遅らせる
      // （mousedown は既定の動作を止めているので、ブラウザもフォーカスを移さない）。その間のキー入力は
      // body に落ち、クリック直後の Enter・文字入力で編集が始まらない。クリックの確定（mouseup）で
      // 同期的にグリッドへフォーカスを移しておく（選択はこの時点で確定済み）。
      editorRef.current?.focus();
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

  // ---- awareness（自分の位置を送る・他の人の位置を描く） ----
  const selectedCell = selection.current?.cell;
  const myPosition = React.useMemo(() => {
    if (embedded || !selectedCell || initial !== "ready") return null;
    const [col, row] = selectedCell;
    const d = display[row];
    const c = columns[col];
    if (d?.kind !== "row" || !c) return null;
    const r = rows.current[d.index];
    if (!r) return null;
    // 擬似列（作成日時など）はスキーマの列ではないので、行だけを知らせる。
    return { record_id: r.id, field: c.field ? c.key : null };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [embedded, selectedCell?.[0], selectedCell?.[1], display, columns, initial]);
  const positionKey = myPosition ? `${myPosition.record_id}\u0001${myPosition.field ?? ""}` : "";
  const sentPosition = React.useRef("");
  React.useEffect(() => {
    if (embedded) return;
    const send = () => {
      sentPosition.current = positionKey;
      postAwareness(tableId, myPosition ?? { record_id: null, field: null }).catch(() => {
        // 位置は付加情報（失敗しても編集・閲覧は続ける）。
      });
    };
    // 選択の移動は少し待ってまとめる（矢印キーの連打で送り続けない）。離脱は前に送っていたときだけ。
    const t = window.setTimeout(() => {
      if (positionKey !== sentPosition.current) send();
    }, 250);
    // 選んでいる間は送り直す（サーバは 30 秒より古い位置を配らない）。
    const hb = myPosition ? window.setInterval(send, AWARENESS_HEARTBEAT_MS) : undefined;
    return () => {
      window.clearTimeout(t);
      if (hb) window.clearInterval(hb);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [positionKey, tableId, embedded]);
  // 画面を離れたら位置を消す（他の人の画面に古い枠を残さない）。
  React.useEffect(() => {
    if (embedded) return;
    return () => {
      if (sentPosition.current) {
        void postAwareness(tableId, { record_id: null, field: null }).catch(() => undefined);
        sentPosition.current = "";
      }
    };
  }, [tableId, embedded]);
  // 送り直しが途絶えた人（タブを閉じた等）は消す。
  React.useEffect(() => {
    if (embedded) return;
    const t = window.setInterval(() => {
      const now = Date.now();
      let changed = false;
      for (const [k, p] of peers.current) {
        if (now - p.seenAt > PEER_TTL_MS) {
          peers.current.delete(k);
          changed = true;
        }
      }
      if (changed) publishPeers();
    }, 5_000);
    return () => window.clearInterval(t);
  }, [embedded, publishPeers]);

  const highlightRegions = React.useMemo(() => {
    if (peers.current.size === 0 || showingStale) return undefined;
    const rowOf = new Map<number, number>();
    display.forEach((d, i) => {
      if (d.kind === "row") rowOf.set(d.index, i);
    });
    const colOf = new Map(columns.map((c, i) => [c.key, i]));
    // "solid" は塗り（透過色）＋枠（不透明の同じ色）。"solid-outline" は枠が背景と混ざって見えにくい。
    const out: { color: string; range: Rectangle; style: "solid" }[] = [];
    for (const p of peers.current.values()) {
      const idx = indexById.current.get(p.recordId);
      const y = idx === undefined ? undefined : rowOf.get(idx);
      if (y === undefined) continue;
      const x = p.field ? colOf.get(p.field) : undefined;
      out.push({
        color: withAlpha(peerColor(p.principal), 0.14),
        range: x === undefined ? { x: 0, y, width: columns.length, height: 1 } : { x, y, width: 1, height: 1 },
        style: "solid",
      });
    }
    return out.length ? out : undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [peerVersion, display, columns, showingStale]);

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
      {initial === "loading" && !showingStale ? (
        <div
          className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-card/70 text-sm text-muted-foreground"
          role="status"
        >
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : null}
      {/* 読み直し・準備中の間は直前の行を薄く残し、上に状態だけを出す（失敗の文言・再試行は出さない）。 */}
      {showingStale ? <div className="pointer-events-none absolute inset-0 z-10 bg-card/55" aria-hidden /> : null}
      {initial === "loading" && showingStale ? (
        <div className="absolute left-1/2 top-12 z-20 inline-flex -translate-x-1/2 items-center gap-1.5 rounded-full border bg-card px-3 py-1.5 text-xs text-muted-foreground shadow-md" role="status">
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : null}
      {initial === "preparing" ? (
        <div
          className={cn(
            "z-20 flex flex-col items-center justify-center gap-2 text-sm text-muted-foreground",
            showingStale
              ? "absolute left-1/2 top-12 w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 rounded-xl border bg-card px-4 py-3 text-center shadow-md"
              : "absolute inset-0 bg-card",
          )}
          role="status"
          data-testid="data-grid-waiting"
        >
          <span className="inline-flex items-center gap-2 text-balance text-foreground">
            <Loader2 className="size-4 shrink-0 animate-spin text-primary" aria-hidden />
            {waiting?.message ?? "並べ替え・絞り込みの準備をしています"}
          </span>
          {waiting?.progress != null ? (
            <span className="h-1.5 w-48 overflow-hidden rounded-full bg-muted" aria-label={`進捗 ${Math.round(waiting.progress * 100)}%`}>
              <span
                className="block h-full rounded-full bg-primary transition-[width] duration-500"
                style={{ width: `${Math.max(4, Math.round(waiting.progress * 100))}%` }}
              />
            </span>
          ) : null}
          <span className="text-xs">準備ができたら自動で表示します</span>
        </div>
      ) : null}
      {initial === "error" ? (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-card text-sm text-muted-foreground">
          <p className="max-w-sm text-balance text-center">{failure ?? "行を読み込めませんでした"}</p>
          <button
            type="button"
            onClick={() => reset()}
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
          onClick={() => reset()}
          data-testid="data-grid-new-rows"
          className="absolute bottom-4 left-1/2 z-20 inline-flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-primary/30 bg-card px-3 py-1.5 text-xs font-medium text-primary shadow-md transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <RefreshCw className="size-3.5" aria-hidden />
          新しいレコード {newRows} 件 · 表示する
        </button>
      ) : null}
      {live && initial === "ready" ? (
        <div
          role="status"
          data-testid="data-grid-live-problem"
          className="absolute bottom-4 left-1/2 z-20 flex w-[min(30rem,calc(100%-2rem))] -translate-x-1/2 items-center gap-2 rounded-xl border bg-card px-3 py-2 text-xs shadow-md"
        >
          <WifiOff className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          <span className="flex-1 text-balance">
            {live.kind === "signed_out"
              ? "ログインが切れたため、他の人の変更の自動反映を止めました。もう一度ログインしてください。"
              : "他の人の変更の自動反映が止まりました。表示中の行は古い可能性があります。"}
          </span>
          {live.kind === "disconnected" ? (
            <button
              type="button"
              onClick={() => reset()}
              className="inline-flex shrink-0 items-center gap-1 rounded-md border px-2 py-1 font-medium text-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <RefreshCw className="size-3.5" aria-hidden />
              読み直す
            </button>
          ) : null}
        </div>
      ) : null}
      <DataEditor
        ref={editorRef}
        theme={glideTheme}
        columns={gridColumns}
        rows={totalRows}
        rowHeight={rowHeight}
        getCellContent={getCellContent}
        onCellEdited={canEdit ? onCellEdited : undefined}
        onCellClicked={onCellClicked}
        onVisibleRegionChanged={onVisibleRegionChanged}
        gridSelection={selection}
        onGridSelectionChange={setSelection}
        highlightRegions={highlightRegions}
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

