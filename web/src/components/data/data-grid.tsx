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
  type ProvideEditorCallbackResult,
  type Rectangle,
  type TextCell,
  type Theme,
} from "@glideapps/glide-data-grid";
import { AlertTriangle, Loader2, Plus, RefreshCw, Trash2, WifiOff } from "lucide-react";
import * as React from "react";
import { flushSync } from "react-dom";

import { resolveColor, useGlideTheme } from "@/components/csv/glide-theme";
import { GridTextEditor } from "@/components/data/grid-text-editor";
import { isRefField, refCellEditor, type RefCellPayload } from "@/components/data/ref-picker";
import { ConfirmDialog } from "@/components/drive/dialogs";
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
  deleteRecord,
  getRecord,
  getViewerContext,
  postAwareness,
  runQuery,
  subscribeRecords,
  updateRecord,
  type RecordSubscription,
  type CountResult,
  type DataQuery,
  type DataRecord,
  type GridView,
  type QueryResult,
} from "@/lib/data-api";
import {
  cellValue,
  displayCell,
  FORMAT_COLORS,
  formatFor,
  formatValue,
  isEditable,
  parseInput,
  userRefIds,
  type ColumnModel,
} from "@/lib/data-view";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";

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
  /// 並べ替えが索引の先頭の一部の文字で決まる（長い値は完全な順にならない・data-platform §3.3）。
  sortPrecision?: "exact" | "prefix" | null;
  /// この並べ替えの組み合わせは複合索引の準備中で、深いページほど遅い（§5.3）。
  sortDegraded?: boolean;
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
  /// 自分の取込の最中（取り込んだ行の出現を「新しいレコード」と数えない・終わったら呼び出し側が読み直す）。
  importing?: boolean;
  /// 行が 1 件も無いときの「レコードを追加」（編集できる人だけ）。
  onAddRecord?: () => void;
};

/// 行のセル（編集を始めた時点の行 id と rev を持たせる）。
///
/// glide は編集を開いた瞬間のセルの内容を編集欄に写し取り、確定時はその写し（`{...cell, data}`）を
/// `onCellEdited` に渡す。行 id と rev をセルに載せておけば、編集中に SSE で行が新しい rev に置き換わっても
/// **編集を始めた時点の rev** で `expected_rev` を送れる（他の人の更新を黙って上書きしない・D-1）。
/// 表示行の位置も編集中に行の出現・消滅でずれうるので、確定先は位置ではなく行 id で引く。
type RowCell = TextCell & {
  rowRef?: { id: string; rev: number };
  /// 参照の列（ユーザー・部署・リンク）: 列の参照名と今の値（編集欄が候補から選ぶ）。
  refField?: string;
  refValue?: RefCellPayload;
};

/// 購読が止まった理由（グリッドの下に案内を出す）。`reconnecting` はサーバに届かない間（停止・再起動中）で、
/// 間隔を空けて自動で繋ぎ直す。
type LiveProblem = { kind: "disconnected" } | { kind: "signed_out" } | { kind: "reconnecting" };

/// サーバに届かないときの繋ぎ直しの間隔（2, 4, 8, 16, 30, 30… 秒）。
const RECONNECT_MS = (attempt: number) => Math.min(30_000, 2_000 * 2 ** attempt);

function groupLabel(r: DataRecord, key: string, col: ColumnModel | undefined): string {
  const s = col
    ? displayCell(r, col, cachedUserName).text
    : formatValue(cellValue(r, key), null, key);
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
    importing,
    onAddRecord,
  },
  ref,
) {
  const importingRef = React.useRef(importing);
  importingRef.current = importing;
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
  /// 先頭からの読み直し（fetchNext より後で定義する reset を、fetchNext の中から呼ぶため）。
  const resetRef = React.useRef<(() => void) | null>(null);
  const fetchNextRef = React.useRef<(() => Promise<void>) | null>(null);
  const lastCursorReset = React.useRef(0);
  /// 最後に知らされた見えている範囲（行の位置）。
  const visibleRegion = React.useRef<Rectangle | null>(null);
  /// 保存中の行（行 id → 保存中の件数・薄く色を付ける）。
  const pending = React.useRef<Map<string, number>>(new Map());
  /// 同じ行の保存は順に送る（行 id → 直前の保存の完了）。自分の保存で進んだ rev（送った rev → 新しい rev）を
  /// 覚えておき、保存の応答より前に同じ行の編集を始めていても**自分の更新だけ**を辿って rev を進める
  /// （自分の直前の保存を「他の人の更新」と取り違えて 409 にしない）。他の人の更新は辿らないので 409 のまま。
  const saveChain = React.useRef<Map<string, Promise<void>>>(new Map());
  const ownAdvance = React.useRef<Map<string, Map<number, number>>>(new Map());
  const followOwn = (id: string, rev: number): number => {
    const adv = ownAdvance.current.get(id);
    let r = rev;
    for (let guard = 0; adv?.has(r) && guard < 1000; guard++) r = adv.get(r)!;
    return r;
  };
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
  /// 編集欄を開いている行（開いた時点の id・rev）。編集中にその行が他の人に更新されたら知らせる。
  const editing = React.useRef<{ id: string; rev: number } | null>(null);
  const [editStale, setEditStale] = React.useState(false);
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
        report({
          sortPrecision: (res.sort_precision as DataGridStatus["sortPrecision"]) ?? null,
          sortDegraded: Boolean(res.sort_degraded),
        });
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
      // 見えている範囲が既に末尾に届いている（行が少ないページ・行の権限で絞られて短いページ）なら、スクロールを
      // 待たずに続きを取る（glide は見える範囲が変わったときしか知らせない）。
      const v = visibleRegion.current;
      if (!done.current && v && v.y + v.height + PREFETCH_ROWS >= rows.current.length) {
        window.setTimeout(() => {
          if (gen === generation.current) void fetchNextRef.current?.();
        }, 0);
      }
    } catch (e) {
      if (gen !== generation.current) return;
      // 続きのページの位置（カーソル）が使えなくなった（サーバの再起動で鍵が変わった・スナップショットの保持
      // 切れ）: 空行のまま止めず、先頭から読み直す（D-9）。古いサーバは code の無い 400 を返すので、先頭ページは
      // 同じ条件で読めていた続きのページの 400 もカーソルの失効とみなす。
      if (
        !first &&
        e instanceof DataApiError &&
        (e.code === "cursor_invalid" || e.code === "snapshot_expired" || e.status === 410 || (e.status === 400 && !e.code)) &&
        // 読み直しても続きが同じ理由で読めない（位置の失効ではない）なら繰り返さない。
        Date.now() - lastCursorReset.current > 15_000
      ) {
        lastCursorReset.current = Date.now();
        loading.current = false;
        toast({
          title: "読み込み位置が古くなったため、先頭から読み直しました",
          description: "サーバーの再起動などで続きの位置が使えなくなりました。",
        });
        resetRef.current?.();
        return;
      }
      // 索引の準備中・承認待ちは画面側の案内（進捗・承認）に任せ、ここは失敗として見せない。
      const waiting = e instanceof DataApiError && (e.indexPreparing || e.approvalRequired);
      if (first) {
        setFailure(
          e instanceof DataApiError && (e.status === 403 || e.status === 404)
            ? "このテーブルを見る権限がありません（共有が外されたか、削除されました）"
            : e instanceof DataApiError && (e.readTimeout || e.status === 400)
              ? // 負荷で時間切れ（少し待てば読める）・条件の誤り（構造化した理由）はその文言を出す。
                e.retryAfterSec
                ? `${e.message}（${e.retryAfterSec} 秒ほど）`
                : e.message
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

  /// 行の出現・消滅（権限の変更で見えなくなった行を含む）の後に件数を取り直す（まとめて 1 回・B-P3-5）。
  const countTimer = React.useRef<ReturnType<typeof setTimeout> | null>(null);
  const refreshCountSoon = (gen: number) => {
    if (countTimer.current) clearTimeout(countTimer.current);
    countTimer.current = setTimeout(() => {
      countTimer.current = null;
      if (gen === generation.current) void fetchCount(gen);
    }, 400);
  };

  const closeStream = React.useRef<RecordSubscription | null>(null);
  /// 非表示のタブで止めている購読の再開位置（`since`）。HTTP/1.1 では同じホストへの接続が 6 本までなので、
  /// 裏のタブが購読を持ち続けると新しいタブが開けなくなる（D-4）。非表示の間は閉じて、表示されたら続きから
  /// 繋ぎ直す（保持期間を過ぎていればサーバが stream.reset を送り、読み直す）。
  const paused = React.useRef<{ since: number | null; gen: number } | null>(null);
  const reconnectTimer = React.useRef<ReturnType<typeof setTimeout> | null>(null);
  const reconnectAttempt = React.useRef(0);
  /// 予約中の繋ぎ直しの位置（非表示になったら予約を止めて、表示されたらここから繋ぐ）。
  const retryAt = React.useRef<{ since: number | null; gen: number } | null>(null);
  const clearReconnect = () => {
    if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
    reconnectTimer.current = null;
  };
  const subscribe = (since: number | null, gen: number) => {
    closeStream.current?.();
    closeStream.current = null;
    clearReconnect();
    if (typeof document !== "undefined" && document.visibilityState === "hidden") {
      paused.current = { since, gen };
      return;
    }
    paused.current = null;
    let opened = false;
    closeStream.current = subscribeRecords(tableId, since, {
      onOpen: () => {
        if (gen !== generation.current) return;
        opened = true;
        reconnectAttempt.current = 0;
        setLive((l) => (l?.kind === "reconnecting" ? null : l));
      },
      onUpserted: ({ record, derived }) => {
        if (gen !== generation.current) return;
        const i = indexById.current.get(record.id);
        if (i === undefined) {
          // 自分の取込で現れた行は数えない（取込が終われば読み直す）。
          if (!importingRef.current) setNewRows((n) => n + 1);
          if (!derived) refreshCountSoon(gen);
          return;
        }
        const cur = rows.current[i];
        // 冪等: 手元より新しい rev だけ（派生値だけの再配信は同じ rev でも適用）。
        if (derived ? record.rev < cur.rev : record.rev <= cur.rev) return;
        rows.current[i] = record;
        // 編集中の行が他の人に更新された（自分の保存の反映は保存中か、自分の保存で進んだ rev かで区別する）:
        // 確定すると競合になるので、編集中のまま知らせる。
        const ed = editing.current;
        if (
          ed &&
          ed.id === record.id &&
          !derived &&
          !pending.current.has(record.id) &&
          record.rev > followOwn(record.id, ed.rev)
        ) {
          setEditStale(true);
        }
        bump();
      },
      onRemoved: ({ record_id }) => {
        if (gen !== generation.current) return;
        // 見えていた行が消えた（削除・権限の変更で不可視）: 手元に無い行でも件数には入っていた。
        refreshCountSoon(gen);
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
      onBulk: (count) => {
        if (gen !== generation.current) return;
        // 取込などでまとめて作られた行: 並び順の途中に差し込まず、件数だけ数えて「表示する」を出す。
        if (!importingRef.current) setNewRows((n) => n + count);
        refreshCountSoon(gen);
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
      onTerminal: (lastId) => {
        if (gen !== generation.current) return;
        closeStream.current = null;
        void diagnoseStream(gen, lastId, opened);
      },
    });
  };

  /// 購読が繋ぎ直せなくなった理由を REST で確かめる（EventSource は状態コードを読めない）。
  ///
  /// - REST も届かない（サーバの停止・再起動中・BFF の 502）: 「再接続しています」を出し、間隔を空けて最後に
  ///   受け取った番号から繋ぎ直す
  /// - REST は読める: 一度は繋がっていた購読が切れた（再起動の直後など）なら 1 回だけすぐ繋ぎ直す。最初から
  ///   繋がらない（購読だけ拒否された）なら止めて「読み直す」を出す（繋ぎ直しの嵐にしない）
  /// - 403・404: 見る権限を失った。401: ログインが切れた
  const diagnoseStream = async (gen: number, lastId: number | null, wasOpen: boolean) => {
    const retryLater = () => {
      setLive({ kind: "reconnecting" });
      clearReconnect();
      retryAt.current = { since: lastId, gen };
      const wait = RECONNECT_MS(reconnectAttempt.current);
      reconnectAttempt.current += 1;
      reconnectTimer.current = setTimeout(() => {
        reconnectTimer.current = null;
        if (gen === generation.current) subscribe(lastId, gen);
      }, wait);
    };
    try {
      await getViewerContext(tableId);
      if (gen !== generation.current) return;
      if (wasOpen) subscribe(lastId, gen);
      else setLive({ kind: "disconnected" });
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
        retryLater();
      }
    }
  };

  const reset = React.useCallback((opts?: { dropStale?: boolean }) => {
    generation.current += 1;
    closeStream.current?.();
    closeStream.current = null;
    paused.current = null;
    clearReconnect();
    reconnectAttempt.current = 0;
    if (opts?.dropStale) stale.current = [];
    else if (rows.current.length) stale.current = rows.current;
    rows.current = [];
    indexById.current = new Map();
    cursor.current = null;
    done.current = false;
    loading.current = false;
    pending.current.clear();
    saveChain.current.clear();
    ownAdvance.current.clear();
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
      clearReconnect();
      if (countTimer.current) clearTimeout(countTimer.current);
    };
  }, [reset]);

  // 非表示のタブは購読を止め、表示されたら続きから繋ぎ直す（D-4）。
  React.useEffect(() => {
    if (embedded) return;
    const onVisibility = () => {
      if (document.visibilityState === "hidden") {
        const sub = closeStream.current;
        if (sub) {
          paused.current = { since: sub.lastEventId(), gen: generation.current };
          sub();
          closeStream.current = null;
        } else if (reconnectTimer.current && retryAt.current) {
          paused.current = retryAt.current;
        }
        clearReconnect();
      } else {
        const p = paused.current;
        if (p && p.gen === generation.current) subscribe(p.since, p.gen);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [embedded, tableId]);

  resetRef.current = () => reset();
  React.useImperativeHandle(ref, () => ({ reload: () => reset() }), [reset]);

  // ---- 表示行（グループ見出しを挟む） ----
  const colByKey = React.useMemo(() => new Map(columns.map((c) => [c.key, c])), [columns]);
  const showingStale = initial !== "ready" && rows.current.length === 0 && stale.current.length > 0;
  const source = showingStale ? stale.current : rows.current;
  // user_ref 列・作成者列の表示名を引く（手元の行に出てきた未解決の id だけ・引けたら描き直す）。
  const [namesVersion, setNamesVersion] = React.useState(0);
  React.useEffect(() => {
    const ids = userRefIds(source, columns).filter((id) => cachedUserName(id) === undefined);
    if (ids.length === 0) return;
    let active = true;
    void resolveUserNames(ids).then((learned) => {
      if (active && learned) setNamesVersion((v) => v + 1);
    });
    return () => {
      active = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, columns, showingStale]);

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
              label: `${col?.label ?? groupKeys[l]}: ${groupLabel(r, groupKeys[l], col)}`,
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
  }, [version, groupKeys, collapsed, colByKey, showingStale, namesVersion]);

  // グループを畳んで表示行が減ったら、見えている範囲が末尾に届いていれば続きを読む（畳んだまま空白に
  // ならない）。
  React.useEffect(() => {
    const v = visibleRegion.current;
    if (!done.current && initial === "ready" && v && v.y + v.height + PREFETCH_ROWS >= display.length) void fetchNext();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [collapsed]);

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
      const shown = displayCell(r, c, cachedUserName);
      const text = shown.text;
      const editable = canEdit && !showingStale && isEditable(c.field);
      const fmt = formatFor(grid, r, c.key);
      const numeric = c.field?.type === "number" || c.field?.result_type === "number";
      const override: Partial<Theme> | undefined = pending.current.has(r.id)
        ? { bgCell: editedBg }
        : fmt
          ? { ...formatTheme.get(fmt.color), ...(fmt.bold ? { baseFontStyle: "600 13px" } : {}) }
          : shown.muted
            ? { textDark: glideTheme.textLight, baseFontStyle: "italic 12px" }
            : undefined;
      const ref = isRefField(c.field);
      const ids = (Array.isArray(v) ? v : v == null ? [] : [v]).filter((x): x is string => typeof x === "string");
      const cell: RowCell = {
        kind: GridCellKind.Text,
        data: Array.isArray(v) ? v.join(", ") : v == null ? "" : String(v),
        displayData: text,
        allowOverlay: editable,
        readonly: !editable,
        contentAlign: numeric ? "right" : undefined,
        themeOverride: override,
        rowRef: { id: r.id, rev: r.rev },
        ...(ref
          ? {
              refField: c.key,
              refValue: {
                ids,
                labels: Object.fromEntries(ids.map((id) => [id, displayCell({ ...r, data: { [c.key]: id } } as DataRecord, c, cachedUserName).text])),
              },
            }
          : {}),
      };
      return cell;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [display, columns, canEdit, grid, collapsed, editedBg, formatTheme, glideTheme, version, source, showingStale, namesVersion],
  );

  const onCellEdited = React.useCallback(
    ([col, row]: Item, cell: EditableGridCell) => {
      if (cell.kind !== GridCellKind.Text) return;
      const c = columns[col];
      if (!c?.field) return;
      // 確定先は編集を始めた時点の行（id）と rev。位置からは引かない（編集中に行がずれうる）。
      const ref = (cell as RowCell).rowRef;
      const d = display[row];
      const i = ref ? indexById.current.get(ref.id) : d?.kind === "row" ? d.index : undefined;
      if (i === undefined) {
        toast({
          variant: "destructive",
          title: "保存できませんでした",
          description: "編集していた行は削除されたか、見られなくなりました。",
        });
        return;
      }
      const current = rows.current[i];
      const expectedRev = ref?.rev ?? current.rev;
      // 比較・巻き戻しは編集を始めた時点の値ではなく今の行（他の人の更新を巻き戻さない）。
      const before = current;
      const parsed = parseInput(cell.data, c.field);
      if (!parsed.ok) {
        toast({ variant: "destructive", title: `「${c.label}」を保存できません`, description: parsed.error });
        return;
      }
      const prevValue = cellValue(before, c.key);
      if (JSON.stringify(prevValue ?? null) === JSON.stringify(parsed.value ?? null)) return;
      // 先に見せる（保存中は薄く色を付ける）。参照の列は選んだ候補の見出しも添える。
      let pickedLabels: Record<string, string> = {};
      if (isRefField(c.field) && cell.data.startsWith("{")) {
        try {
          pickedLabels = (JSON.parse(cell.data) as RefCellPayload).labels ?? {};
        } catch {
          pickedLabels = {};
        }
      }
      rows.current[i] = {
        ...before,
        data: { ...(before.data as object), [c.key]: parsed.value },
        ...(c.field.type === "record_ref"
          ? { link_labels: { ...((before.link_labels ?? {}) as Record<string, string>), ...pickedLabels } }
          : {}),
      };
      const id = before.id;
      pending.current.set(id, (pending.current.get(id) ?? 0) + 1);
      bump();
      const gen = generation.current;
      const run = (saveChain.current.get(id) ?? Promise.resolve())
        .then(async () => {
          const rev = followOwn(id, expectedRev);
          const saved = await updateRecord(tableId, id, { [c.key]: parsed.value }, rev);
          if (gen !== generation.current) return;
          const adv = ownAdvance.current.get(id) ?? new Map<number, number>();
          adv.set(rev, saved.rev);
          ownAdvance.current.set(id, adv);
          const j = indexById.current.get(saved.id);
          if (j !== undefined && saved.rev >= rows.current[j].rev) rows.current[j] = saved;
        })
        .catch(async (e: unknown) => {
          if (gen !== generation.current) return;
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
          const n = (pending.current.get(id) ?? 1) - 1;
          if (n > 0) pending.current.set(id, n);
          else pending.current.delete(id);
          if (saveChain.current.get(id) === run) saveChain.current.delete(id);
          bump();
        });
      saveChain.current.set(id, run);
    },
    [display, columns, tableId, bump],
  );

  /// 編集欄を開く（glide は開くときだけ呼ぶ）。確定の値を入力欄から読む編集欄を使い、開いた行を覚える
  /// （描画中に呼ばれるので state は触らず ref だけ・知らせの解除は確定／取消の `onFinishedEditing`）。
  const columnsRef = React.useRef(columns);
  columnsRef.current = columns;
  const provideEditor = React.useCallback((cell: GridCell) => {
    if (cell.kind !== GridCellKind.Text) return undefined;
    const rc = cell as RowCell;
    editing.current = rc.rowRef ?? null;
    // 参照の列は候補から選ぶ編集欄（A-6）。
    const field = rc.refField ? columnsRef.current.find((c) => c.key === rc.refField)?.field : null;
    if (field && rc.refValue) return refCellEditor(field, rc.refValue) as ProvideEditorCallbackResult<GridCell>;
    return GridTextEditor as ProvideEditorCallbackResult<GridCell>;
  }, []);
  const onFinishedEditing = React.useCallback(() => {
    editing.current = null;
    setEditStale(false);
  }, []);

  const onSelectionChange = React.useCallback((s: GridSelection) => {
    flushSync(() => setSelection(s));
  }, []);

  // ---- 行の削除（行番号で行を選んで「削除」・A-4） ----
  const selectedRecords = React.useMemo(() => {
    if (embedded || !canEdit || showingStale) return [];
    const out: DataRecord[] = [];
    for (const y of selection.rows) {
      const d = display[y];
      if (d?.kind !== "row") continue;
      const r = rows.current[d.index];
      if (r) out.push(r);
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection.rows, display, embedded, canEdit, showingStale, version]);
  const [confirmDelete, setConfirmDelete] = React.useState<DataRecord[] | null>(null);
  const deleteRecords = async (targets: DataRecord[]) => {
    const gen = generation.current;
    const results = await Promise.allSettled(targets.map((r) => deleteRecord(tableId, r.id, r.rev)));
    if (gen !== generation.current) return;
    const gone = new Set<string>();
    let conflicts = 0;
    let firstError: string | null = null;
    results.forEach((res, i) => {
      if (res.status === "fulfilled") gone.add(targets[i].id);
      else if (res.reason instanceof DataApiError && res.reason.revConflict) conflicts += 1;
      else firstError ??= res.reason instanceof Error ? res.reason.message : String(res.reason);
    });
    if (gone.size) {
      rows.current = rows.current.filter((r) => !gone.has(r.id));
      rebuildIndex();
      report({ loaded: rows.current.length });
      setSelection({ columns: CompactSelection.empty(), rows: CompactSelection.empty() });
      refreshCountSoon(gen);
      bump();
    }
    if (conflicts || firstError) {
      toast({
        variant: "destructive",
        title: gone.size ? `${gone.size} 件を削除し、${targets.length - gone.size} 件は削除できませんでした` : "削除できませんでした",
        description: conflicts
          ? "他の人が先に更新した行があります。内容を確かめてからもう一度削除してください。"
          : (firstError ?? undefined),
      });
      if (conflicts) resetRef.current?.();
    } else {
      toast({ title: `${gone.size.toLocaleString("ja-JP")} 件のレコードを削除しました` });
    }
  };
  /// Delete キー: 行を選んでいるときはセルを空にせず、行の削除を確かめる（行ごと空にしない）。
  const onDelete = React.useCallback(
    (sel: GridSelection): boolean | GridSelection => {
      if (sel.rows.length > 0) {
        if (selectedRecords.length) setConfirmDelete(selectedRecords);
        return false;
      }
      return true;
    },
    [selectedRecords],
  );

  fetchNextRef.current = fetchNext;
  const onVisibleRegionChanged = React.useCallback(
    (r: Rectangle) => {
      visibleRegion.current = r;
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
        // 0 行: 絞り込みが掛かっていれば「条件に合う行が無い」、無ければテーブルが空（D-13）。
        query.filter ? (
          <div className="pointer-events-none absolute inset-x-0 top-24 z-10 text-center text-sm text-muted-foreground" data-testid="data-grid-empty">
            条件に合うレコードはありません
          </div>
        ) : (
          <div
            className="absolute left-1/2 top-20 z-10 flex -translate-x-1/2 flex-col items-center gap-2.5 rounded-xl border bg-card px-6 py-5 text-center text-sm text-muted-foreground shadow-xs"
            data-testid="data-grid-empty"
          >
            <p>まだレコードがありません</p>
            {onAddRecord && canEdit ? (
              <button
                type="button"
                onClick={onAddRecord}
                className="inline-flex items-center gap-1.5 rounded-md border bg-card px-3 py-1.5 text-xs font-medium text-foreground shadow-xs hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Plus className="size-3.5" aria-hidden />
                最初のレコードを追加
              </button>
            ) : null}
          </div>
        )
      ) : null}
      {selectedRecords.length > 0 && !confirmDelete ? (
        <div
          role="toolbar"
          aria-label="選択した行の操作"
          data-testid="data-grid-selection"
          className={cn(
            "absolute left-4 z-20 flex items-center gap-2 rounded-xl border bg-card px-3 py-1.5 text-xs shadow-md",
            newRows > 0 || live ? "bottom-16" : "bottom-4",
          )}
        >
          <span className="tabular-nums">{selectedRecords.length.toLocaleString("ja-JP")} 件を選択中</span>
          <button
            type="button"
            onClick={() => setConfirmDelete(selectedRecords)}
            data-testid="data-grid-delete-rows"
            className="inline-flex items-center gap-1 rounded-md border border-destructive/40 px-2 py-1 font-medium text-destructive hover:bg-destructive/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Trash2 className="size-3.5" aria-hidden />
            削除
          </button>
          <button
            type="button"
            onClick={() => setSelection({ columns: CompactSelection.empty(), rows: CompactSelection.empty() })}
            className="rounded-md px-2 py-1 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            選択を外す
          </button>
        </div>
      ) : null}
      <ConfirmDialog
        open={confirmDelete !== null}
        onOpenChange={(o) => !o && setConfirmDelete(null)}
        title={`${(confirmDelete?.length ?? 0).toLocaleString("ja-JP")} 件のレコードを削除しますか？`}
        description="削除したレコードは元に戻せません。リンクしている他のレコードからも外れます。"
        confirmLabel="削除する"
        destructive
        onConfirm={async () => {
          if (confirmDelete) await deleteRecords(confirmDelete);
        }}
      />
      {editStale ? (
        <div
          role="alert"
          data-testid="data-grid-edit-stale"
          className={cn(
            // 編集欄（行の位置）に重ならないよう下端に出す（「新しいレコード」の案内があればその上）。
            "absolute left-1/2 z-20 flex w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 items-start gap-2 rounded-xl border border-destructive/40 bg-card px-3 py-2 text-xs shadow-md",
            newRows > 0 || live ? "bottom-16" : "bottom-4",
          )}
        >
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
          <span className="flex-1 text-balance">
            編集中に他の人がこの行を更新しました。確定しても上書きはせず、最新の内容を読み込み直します（Esc で編集をやめられます）。
          </span>
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
          新しいレコード {newRows.toLocaleString("ja-JP")} 件 · 表示する
        </button>
      ) : null}
      {live && initial === "ready" ? (
        <div
          role="status"
          data-testid="data-grid-live-problem"
          className="absolute bottom-4 left-1/2 z-20 flex w-[min(30rem,calc(100%-2rem))] -translate-x-1/2 items-center gap-2 rounded-xl border bg-card px-3 py-2 text-xs shadow-md"
        >
          {live.kind === "reconnecting" ? (
            <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
          ) : (
            <WifiOff className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          )}
          <span className="flex-1 text-balance">
            {live.kind === "signed_out"
              ? "ログインが切れたため、他の人の変更の自動反映を止めました。もう一度ログインしてください。"
              : live.kind === "reconnecting"
                ? "サーバーとの接続が切れました。再接続しています（表示中の行は古い可能性があります）。"
                : "他の人の変更の自動反映が止まりました。表示中の行は古い可能性があります。"}
          </span>
          {live.kind !== "signed_out" ? (
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
        provideEditor={canEdit ? provideEditor : undefined}
        onFinishedEditing={canEdit ? onFinishedEditing : undefined}
        // 選択中のセルをもう一度クリックしても編集欄を開かない（開いたまま Enter を押すと「変更なしで確定 →
        // 1 つ下へ移動」になり、続けて打った文字が下の行に保存されていた・D-10）。編集は Enter・ダブルクリック・
        // 文字の入力で始める（表計算と同じ）。
        cellActivationBehavior="double-click"
        onCellClicked={onCellClicked}
        onVisibleRegionChanged={onVisibleRegionChanged}
        gridSelection={selection}
        // 選択は同期で確定する（クリック直後の Enter・打鍵が、選択の反映前の描画に届いて取りこぼされない）。
        onGridSelectionChange={onSelectionChange}
        onDelete={canEdit && !embedded ? onDelete : undefined}
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
        // 行番号のクリックで行を選ぶ（選んだ行を削除できる・A-4）。
        rowMarkers={embedded ? "none" : canEdit ? "clickable-number" : "number"}
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

