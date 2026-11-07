"use client";

/// SQL の結果の表（#978）。テーブルの画面と同じ glide-data-grid（仮想化・canvas）で描き、見えている範囲が
/// 末尾に近づいたら続きのページを読む（無限スクロール）。行は読み取り専用で、範囲を選んでコピーできる。

import {
  DataEditor,
  GridCellKind,
  type GridCell,
  type GridColumn,
  type Item,
  type Rectangle,
} from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { Loader2 } from "lucide-react";
import * as React from "react";

import { useGlideTheme } from "@/components/csv/glide-theme";
import { cellText, numericColumns } from "@/components/data/sql-result";
import type { SqlQueryResult } from "@/lib/data-api";

/// 行の高さ（テーブルの画面の「短い」と同じ）。
const ROW_PX = 34;
/// 末尾のこの行数手前まで見えたら続きを読む。
const PREFETCH_ROWS = 20;
/// 列幅の見積もりに使う行数（全行を測らない）。
const WIDTH_SAMPLE_ROWS = 100;

/// 文字列の表示幅の見積もり（全角は半角のおよそ倍）。
function textWidth(s: string, px: number): number {
  let w = 0;
  for (const ch of s) w += (ch.codePointAt(0) ?? 0) > 0x2e7f ? px : px * 0.58;
  return w;
}

function columnWidth(title: string, result: SqlQueryResult, col: number): number {
  let w = textWidth(title, 12);
  for (const row of result.rows.slice(0, WIDTH_SAMPLE_ROWS)) w = Math.max(w, textWidth(cellText(row[col]), 13));
  return Math.round(Math.min(360, Math.max(88, w + 32)));
}

export function SqlResultGrid({
  result,
  hasMore,
  loadingMore,
  onLoadMore,
}: {
  result: SqlQueryResult;
  /// 続きのページがある（LIMIT を書かなかった問い合わせ）。
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
}) {
  const { theme } = useGlideTheme();
  const numeric = React.useMemo(() => numericColumns(result), [result]);
  // 列幅は列の並びが変わったとき（新しい問い合わせ）だけ見積もり直す。続きのページでは変えない。
  const columnsKey = result.columns.join("\u0000");
  const [widths, setWidths] = React.useState<number[]>([]);
  const [measuredKey, setMeasuredKey] = React.useState<string | null>(null);
  if (measuredKey !== columnsKey) {
    setMeasuredKey(columnsKey);
    setWidths(result.columns.map((c, i) => columnWidth(c, result, i)));
  }
  const columns: GridColumn[] = React.useMemo(
    () => result.columns.map((title, i) => ({ id: String(i), title, width: widths[i] ?? 120 })),
    [result.columns, widths],
  );

  const rows = result.rows;
  const getCellContent = React.useCallback(
    ([col, row]: Item): GridCell => {
      const v = rows[row]?.[col];
      const align = numeric[col] ? ("right" as const) : undefined;
      if (v === null || v === undefined) {
        // 伏せた集計と値なしの集計はどちらも NULL なので、セルでは理由を断定しない（結果の上の印で知らせる）。
        return {
          kind: GridCellKind.Text,
          data: "",
          displayData: "—",
          allowOverlay: false,
          readonly: true,
          contentAlign: align,
          themeOverride: { textDark: theme.textLight },
        };
      }
      return {
        kind: GridCellKind.Text,
        data: typeof v === "number" ? String(v) : cellText(v),
        displayData: cellText(v),
        allowOverlay: true,
        readonly: true,
        contentAlign: align,
      };
    },
    [rows, numeric, theme.textLight],
  );

  // 見えている範囲が末尾に近づいたら続きを読む。読み終わった直後にまだ末尾が見えていれば（短いページ）続けて読む。
  const visible = React.useRef<Rectangle | null>(null);
  const maybeLoad = React.useCallback(() => {
    const v = visible.current;
    if (!hasMore || loadingMore || !v) return;
    if (v.y + v.height + PREFETCH_ROWS >= rows.length) onLoadMore();
  }, [hasMore, loadingMore, rows.length, onLoadMore]);
  React.useEffect(() => {
    maybeLoad();
  }, [maybeLoad]);

  return (
    <div
      className="relative h-full min-h-0 overflow-hidden rounded-lg border bg-card"
      data-testid="sql-result"
      data-rows={rows.length}
      data-more={hasMore ? "1" : "0"}
    >
      <DataEditor
        theme={theme}
        columns={columns}
        rows={rows.length}
        rowHeight={ROW_PX}
        headerHeight={36}
        getCellContent={getCellContent}
        onVisibleRegionChanged={(r) => {
          visible.current = r;
          maybeLoad();
        }}
        onColumnResize={(c, w) => setWidths((prev) => prev.map((x, i) => (String(i) === c.id ? w : x)))}
        rowMarkers="number"
        columnSelect="none"
        getCellsForSelection
        smoothScrollX
        smoothScrollY
        width="100%"
        height="100%"
      />
      {loadingMore ? (
        <div
          className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center"
          role="status"
          data-testid="sql-loading-more"
        >
          <span className="inline-flex items-center gap-1.5 rounded-full border bg-popover px-3 py-1 text-xs text-muted-foreground shadow-[var(--elevation-xs)]">
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
            続きを読み込み中…
          </span>
        </div>
      ) : null}
      <div id="portal" style={{ position: "fixed", left: 0, top: 0, zIndex: 9999 }} />
    </div>
  );
}
