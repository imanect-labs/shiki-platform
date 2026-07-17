"use client";

/// 下書き CSV のローカルグリッド（Task 11.11・Glide Data Grid）。
///
/// [`CsvGrid`]（/csv/[id]・サーバページング＋パッチ保存）と違い、**クライアント内の下書き
/// データ**（下書きストアの CSV 文字列）を直接編集する。編集のたびに親（下書き画面）が
/// ストアへ書き戻す（source=user）。末尾行クリックで行を追加できる。

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

import { useGlideTheme } from "@/components/csv/glide-theme";

export function CsvDraftGrid({
  header,
  rows,
  onEdit,
  onAppendRow,
}: {
  /// ヘッダ行（列名）。
  header: string[];
  /// データ行（ヘッダ除く）。
  rows: string[][];
  /// セル編集（row=データ行 index・col=列 index）。
  onEdit: (row: number, col: number, value: string) => void;
  /// 末尾に空行を追加。
  onAppendRow: () => void;
}) {
  const { theme: glideTheme } = useGlideTheme();

  const columns: GridColumn[] = React.useMemo(
    () => header.map((title, i) => ({ title: title || `列${i + 1}`, id: `${i}`, width: 160 })),
    [header],
  );

  const getCellContent = React.useCallback(
    ([col, row]: Item): GridCell => {
      const raw = rows[row]?.[col] ?? "";
      return {
        kind: GridCellKind.Text,
        data: raw,
        displayData: raw,
        allowOverlay: true,
      };
    },
    [rows],
  );

  const onCellEdited = React.useCallback(
    ([col, row]: Item, newValue: EditableGridCell) => {
      if (newValue.kind !== GridCellKind.Text) return;
      onEdit(row, col, newValue.data);
    },
    [onEdit],
  );

  return (
    <div
      className="csv-grid h-full w-full overflow-hidden rounded-lg border border-border shadow-xs"
      data-testid="csv-draft-grid"
    >
      <DataEditor
        theme={glideTheme}
        columns={columns}
        rows={rows.length}
        getCellContent={getCellContent}
        onCellEdited={onCellEdited}
        onRowAppended={onAppendRow}
        trailingRowOptions={{ sticky: true, tint: true }}
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
}
