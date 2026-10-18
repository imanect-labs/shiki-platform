"use client";

/// CSV グリッドエディタ（Task 11P.8・Glide Data Grid）。
///
/// 仮想化＋ページ取得で**無限スクロール**（全量ダウンロードしない）。セル編集はローカルに
/// 蓄積し、保存（明示）で rev 付きパッチ送信 → 楽観ロック（409 は衝突ダイアログ）。
///
/// スマホ（タッチ・狭い幅）では、セルを選ぶと「行の詳細」を出し、下からのシートで 1 行の全列を
/// 縦に並べて読める・直せるようにする（横に長い表をセル単位で追わなくてよい）。

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
} from "@glideapps/glide-data-grid";
import { Rows3, Sparkles } from "lucide-react";
import * as React from "react";

import { useGlideTheme } from "@/components/csv/glide-theme";
import {
  Sheet,
  SheetBody,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { useMediaQuery } from "@/hooks/use-media-query";
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
  /// 選択→AI 指示（Task 11.10）。範囲選択時に「AI に依頼」を出し、TSV 抜粋と範囲を渡す。
  onAskAi?: (selection: {
    excerpt: string;
    rows: [number, number];
    cols: [number, number];
  }) => void;
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
  { nodeId, columns, totalRows, editable, onDirtyChange, onAskAi },
  ref,
) {
  const { theme: glideTheme, editedBg } = useGlideTheme();
  const cache = React.useRef<PageCache>(new Map());
  const fetching = React.useRef<Set<number>>(new Set());
  // セル編集の上書き（"row,col" → 新値）。保存でパッチへ変換。
  const edits = React.useRef<Map<string, string>>(new Map());
  const patches = React.useRef<PatchOp[]>([]);
  const [version, setVersion] = React.useState(0);
  const bump = React.useCallback(() => setVersion((v) => v + 1), []);
  // 数値列（右寄せ表示・表計算ソフトの体裁）。最初のページのサンプルから判定する。
  const [numericCols, setNumericCols] = React.useState<ReadonlySet<number>>(new Set());
  // 範囲選択（選択→AI 指示・Task 11.10）。
  const [selection, setSelection] = React.useState<GridSelection>({
    columns: CompactSelection.empty(),
    rows: CompactSelection.empty(),
  });
  const selectionRange = selection.current?.range ?? null;
  // スマホの行詳細シート（開いている行 index）。
  const touchNarrow = useMediaQuery("(pointer: coarse) and (max-width: 767.98px)");
  const [detailRow, setDetailRow] = React.useState<number | null>(null);

  /// 1 行ぶんの現在値（キャッシュ済みセル＋未保存編集）。
  const readRow = (row: number): string[] => {
    const page = Math.floor(row / PAGE_SIZE);
    const localRow = cache.current.get(page)?.[row - page * PAGE_SIZE];
    return columns.map((_, c) => edits.current.get(`${row},${c}`) ?? localRow?.[c] ?? "");
  };

  /// 選択範囲の TSV 抜粋（キャッシュ済みセル＋未保存編集を反映・上限 50×20 セル）。
  const buildSelectionInfo = (range: {
    x: number;
    y: number;
    width: number;
    height: number;
  }): { excerpt: string; rows: [number, number]; cols: [number, number] } => {
    const maxRows = Math.min(range.height, 50);
    const maxCols = Math.min(range.width, 20);
    const lines: string[] = [
      columns.slice(range.x, range.x + maxCols).join("\t"),
    ];
    for (let r = range.y; r < range.y + maxRows; r += 1) {
      const page = Math.floor(r / PAGE_SIZE);
      const pageRows = cache.current.get(page);
      const localRow = pageRows?.[r - page * PAGE_SIZE];
      const cells: string[] = [];
      for (let c = range.x; c < range.x + maxCols; c += 1) {
        cells.push(edits.current.get(`${r},${c}`) ?? localRow?.[c] ?? "");
      }
      lines.push(cells.join("\t"));
    }
    return {
      excerpt: lines.join("\n"),
      rows: [range.y, range.y + range.height - 1],
      cols: [range.x, range.x + range.width - 1],
    };
  };

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
        // 未保存の編集セルは薄い primary で塗り、保存前に「何が変わったか」を可視化する。
        themeOverride: edited !== undefined ? { bgCell: editedBg } : undefined,
      };
    },
    // version をデップスに入れて再取得後に再評価させる。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [editable, fetchPage, version, numericCols, editedBg],
  );

  const commitCell = React.useCallback(
    (col: number, row: number, value: string) => {
      if (!editable) return;
      const key = `${row},${col}`;
      edits.current.set(key, value);
      patches.current.push({ op: "cell_update", row, col, value });
      onDirtyChange?.(true);
      bump();
    },
    [editable, onDirtyChange, bump],
  );

  const onCellEdited = React.useCallback(
    ([col, row]: Item, newValue: EditableGridCell) => {
      if (newValue.kind !== GridCellKind.Text) return;
      commitCell(col, row, newValue.data);
    },
    [commitCell],
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
    <div
      className="csv-grid relative h-full w-full overflow-hidden rounded-lg border border-border shadow-xs"
      data-testid="csv-grid"
    >
      <DataEditor
        theme={glideTheme}
        columns={gridColumns}
        rows={totalRows}
        getCellContent={getCellContent}
        onCellEdited={editable ? onCellEdited : undefined}
        gridSelection={selection}
        onGridSelectionChange={setSelection}
        rowMarkers="number"
        smoothScrollX
        smoothScrollY
        width="100%"
        height="100%"
        getCellsForSelection
      />
      {/* 選択→AI 指示（Task 11.10）: 範囲選択時にフロートボタンを出す。スマホは行の詳細も。 */}
      {selectionRange && (onAskAi || touchNarrow) ? (
        <div className="absolute right-3 top-3 z-10 flex items-center gap-1.5 max-md:bottom-[max(0.75rem,env(safe-area-inset-bottom))] max-md:right-1/2 max-md:top-auto max-md:translate-x-1/2">
          {touchNarrow ? (
            <button
              type="button"
              data-testid="csv-row-detail"
              onClick={() => setDetailRow(selectionRange.y)}
              className="inline-flex h-10 items-center gap-1.5 whitespace-nowrap rounded-full border border-border/60 bg-card px-3.5 text-[13px] font-medium text-foreground shadow-md transition-colors active:bg-accent"
            >
              <Rows3 className="size-4" aria-hidden />
              行の詳細
            </button>
          ) : null}
          {onAskAi ? (
            <button
              type="button"
              data-testid="csv-ask-ai"
              onClick={() => onAskAi(buildSelectionInfo(selectionRange))}
              className="inline-flex items-center gap-1 whitespace-nowrap rounded-full border border-border/60 bg-card px-3 py-1.5 text-xs font-medium text-primary shadow-sm transition-colors hover:bg-accent max-md:h-10 max-md:gap-1.5 max-md:px-3.5 max-md:text-[13px] max-md:shadow-md"
            >
              <Sparkles className="size-3.5" aria-hidden />
              <span className="max-md:hidden">選択範囲を </span>AI に依頼
            </button>
          ) : null}
        </div>
      ) : null}
      <CsvRowSheet
        row={detailRow}
        totalRows={totalRows}
        columns={columns}
        values={detailRow != null ? readRow(detailRow) : []}
        editable={editable}
        onChangeRow={setDetailRow}
        onCommit={commitCell}
      />
      {/* Glide のオーバーレイエディタ用ポータル。 */}
      <div id="portal" style={{ position: "fixed", left: 0, top: 0, zIndex: 9999 }} />
    </div>
  );
});

/// スマホの行詳細（1 行の全列を縦に並べる）。編集者は値を直せる（確定はフォーカスを外したとき）。
function CsvRowSheet({
  row,
  totalRows,
  columns,
  values,
  editable,
  onChangeRow,
  onCommit,
}: {
  row: number | null;
  totalRows: number;
  columns: string[];
  values: string[];
  editable: boolean;
  onChangeRow: (row: number | null) => void;
  onCommit: (col: number, row: number, value: string) => void;
}) {
  return (
    <Sheet open={row != null} onOpenChange={(open) => !open && onChangeRow(null)}>
      <SheetContent side="bottom" className="max-h-[80dvh]" data-testid="csv-row-sheet">
        <SheetHeader className="pr-14">
          <SheetTitle>{row != null ? `${row + 1} 行目` : "行"}</SheetTitle>
          <SheetDescription>
            {columns.length} 列{editable ? "・値を直すと未保存の編集になります" : ""}
          </SheetDescription>
        </SheetHeader>
        {row != null ? (
          <SheetBody className="px-4">
            <dl className="flex flex-col gap-3">
              {columns.map((col, c) => (
                <div key={`${row}:${c}`} className="flex flex-col gap-1">
                  <dt className="break-all text-[12px] font-medium text-muted-foreground">{col}</dt>
                  <dd>
                    {editable ? (
                      <input
                        defaultValue={values[c] ?? ""}
                        onBlur={(e) => {
                          if (e.target.value !== (values[c] ?? "")) onCommit(c, row, e.target.value);
                        }}
                        aria-label={col}
                        enterKeyHint="next"
                        className="h-11 w-full rounded-md border border-input bg-background px-3 text-base outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      />
                    ) : (
                      <p className="whitespace-pre-wrap break-words text-[15px]">
                        {values[c] || <span className="text-muted-foreground/60">（空）</span>}
                      </p>
                    )}
                  </dd>
                </div>
              ))}
            </dl>
          </SheetBody>
        ) : null}
        {row != null ? (
          <div className="flex items-center justify-between gap-2 border-t px-4 py-2">
            <button
              type="button"
              disabled={row <= 0}
              onClick={() => onChangeRow(row - 1)}
              className="h-11 rounded-full px-4 text-sm font-medium text-foreground/80 active:bg-accent disabled:opacity-40"
            >
              ‹ 前の行
            </button>
            <span className="text-xs tabular-nums text-muted-foreground">
              {row + 1} / {totalRows}
            </span>
            <button
              type="button"
              disabled={row >= totalRows - 1}
              onClick={() => onChangeRow(row + 1)}
              className="h-11 rounded-full px-4 text-sm font-medium text-foreground/80 active:bg-accent disabled:opacity-40"
            >
              次の行 ›
            </button>
          </div>
        ) : null}
      </SheetContent>
    </Sheet>
  );
}
