"use client";

/// CSV グリッドエディタ（Task 11P.8・Glide Data Grid）。
///
/// 仮想化＋ページ取得で**無限スクロール**（全量ダウンロードしない）。セル編集はローカルに
/// 蓄積し、保存（明示）で rev 付きパッチ送信 → 楽観ロック（409 は衝突ダイアログ）。

import "@glideapps/glide-data-grid/dist/index.css";

import {
  DataEditor,
  GridCellKind,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type Item,
} from "@glideapps/glide-data-grid";
import * as React from "react";

import { getRows, type PatchOp, type TableResponse } from "@/lib/tabular-api";

const PAGE_SIZE = 1_000;

export interface CsvGridHandle {
  /// 蓄積したパッチ操作を取り出す（保存後にクリアする）。
  takePatches: () => PatchOp[];
  hasPatches: () => boolean;
  /// 全キャッシュを破棄して再取得する（保存成功/衝突リロード後）。
  reset: () => void;
}

interface Props {
  nodeId: string;
  columns: string[];
  totalRows: number;
  editable: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}

/// ページキャッシュ（行 index → セル値配列）。取得中の行は undefined。
type PageCache = Map<number, Array<Array<string | null>>>;

/// サンプル行から数値列を判定する（非空セルが 1 つ以上あり、その全てが有限数）。
/// カンマ区切りの桁（1,250,000）も数値扱いにする。全列 VARCHAR ロードのため値で推定する。
function detectNumericCols(rows: Array<Array<string | null>>, ncols: number): ReadonlySet<number> {
  const nums = new Set<number>();
  for (let c = 0; c < ncols; c++) {
    let sawValue = false;
    let allNumeric = true;
    for (const row of rows) {
      const v = row[c];
      if (v == null || v === "") continue;
      sawValue = true;
      if (!Number.isFinite(Number(v.replace(/,/g, "")))) {
        allNumeric = false;
        break;
      }
    }
    if (sawValue && allNumeric) nums.add(c);
  }
  return nums;
}

export const CsvGrid = React.forwardRef<CsvGridHandle, Props>(function CsvGrid(
  { nodeId, columns, totalRows, editable, onDirtyChange },
  ref,
) {
  const cache = React.useRef<PageCache>(new Map());
  const fetching = React.useRef<Set<number>>(new Set());
  // セル編集の上書き（"row,col" → 新値）。保存でパッチへ変換。
  const edits = React.useRef<Map<string, string>>(new Map());
  const patches = React.useRef<PatchOp[]>([]);
  const [version, setVersion] = React.useState(0);
  const bump = React.useCallback(() => setVersion((v) => v + 1), []);
  // 数値列（右寄せ表示・表計算ソフトの体裁）。最初のページのサンプルから判定する。
  const [numericCols, setNumericCols] = React.useState<ReadonlySet<number>>(new Set());

  const gridColumns: GridColumn[] = React.useMemo(
    () => columns.map((title) => ({ title, id: title, width: 160 })),
    [columns],
  );

  const fetchPage = React.useCallback(
    (page: number) => {
      if (cache.current.has(page) || fetching.current.has(page)) return;
      fetching.current.add(page);
      getRows(nodeId, page * PAGE_SIZE)
        .then((res: TableResponse) => {
          cache.current.set(page, res.rows);
          // 先頭ページのサンプルで数値列を判定（非空セルが全て数値なら右寄せ）。
          if (page === 0) setNumericCols(detectNumericCols(res.rows, columns.length));
          bump();
        })
        .catch(() => {
          // 取得失敗時は空ページを入れてローディングループを止める（行は空表示）。
          cache.current.set(page, []);
          bump();
        })
        .finally(() => fetching.current.delete(page));
    },
    [nodeId, bump, columns.length],
  );

  const getCellContent = React.useCallback(
    ([col, row]: Item): GridCell => {
      const page = Math.floor(row / PAGE_SIZE);
      const key = `${row},${col}`;
      const edited = edits.current.get(key);
      const rows = cache.current.get(page);
      if (rows === undefined) {
        fetchPage(page);
        return {
          kind: GridCellKind.Loading,
          allowOverlay: false,
        };
      }
      const localRow = rows[row - page * PAGE_SIZE];
      const raw = edited ?? localRow?.[col] ?? "";
      return {
        kind: GridCellKind.Text,
        data: raw,
        displayData: raw,
        allowOverlay: editable,
        readonly: !editable,
        contentAlign: numericCols.has(col) ? "right" : undefined,
      };
    },
    // version をデップスに入れて再取得後に再評価させる。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [editable, fetchPage, version, numericCols],
  );

  const onCellEdited = React.useCallback(
    ([col, row]: Item, newValue: EditableGridCell) => {
      if (!editable || newValue.kind !== GridCellKind.Text) return;
      const key = `${row},${col}`;
      edits.current.set(key, newValue.data);
      patches.current.push({ op: "cell_update", row, col, value: newValue.data });
      onDirtyChange?.(true);
      bump();
    },
    [editable, onDirtyChange, bump],
  );

  React.useImperativeHandle(ref, () => ({
    takePatches: () => {
      const p = patches.current;
      patches.current = [];
      edits.current.clear();
      onDirtyChange?.(false);
      return p;
    },
    hasPatches: () => patches.current.length > 0,
    reset: () => {
      cache.current.clear();
      fetching.current.clear();
      edits.current.clear();
      patches.current = [];
      onDirtyChange?.(false);
      bump();
    },
  }));

  return (
    <div className="csv-grid h-full w-full" data-testid="csv-grid">
      <DataEditor
        columns={gridColumns}
        rows={totalRows}
        getCellContent={getCellContent}
        onCellEdited={editable ? onCellEdited : undefined}
        rowMarkers="number"
        smoothScrollX
        smoothScrollY
        width="100%"
        height="100%"
        getCellsForSelection
      />
      {/* Glide のオーバーレイエディタ用ポータル。 */}
      <div id="portal" style={{ position: "fixed", left: 0, top: 0, zIndex: 9999 }} />
    </div>
  );
});
