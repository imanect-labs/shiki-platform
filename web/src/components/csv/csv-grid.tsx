"use client";

/// CSV グリッドエディタ（Task 11P.8・Glide Data Grid）。
///
/// 仮想化＋ページ取得で**無限スクロール**（全量ダウンロードしない）。セル編集はローカルに
/// 蓄積し、保存（明示）で rev 付きパッチ送信 → 楽観ロック（409 は衝突ダイアログ）。

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
  type Theme,
} from "@glideapps/glide-data-grid";
import { Sparkles } from "lucide-react";
import { useTheme } from "next-themes";
import * as React from "react";

import { getRows, type PatchOp, type TableResponse } from "@/lib/tabular-api";

/// ⚠️ glide-data-grid の内部カラーパーサは oklch()/oklab()/color-mix() を解釈できず黒に
/// フォールバックする（ダークでは黒がたまたま馴染み、ライトで破綻する）。canvas.fillStyle は
/// oklch を rgb へ正規化してくれない（そのまま保持する）ため、1px 実際に塗って getImageData で
/// sRGB の RGBA を読み戻す＝どの色空間の入力でも確実に "rgba(r,g,b,a)" へ変換する。
let _colorCtx: CanvasRenderingContext2D | null = null;
function resolveColor(input: string): string {
  if (!input) return input;
  if (!_colorCtx) {
    const c = document.createElement("canvas");
    c.width = c.height = 1;
    _colorCtx = c.getContext("2d", { willReadFrequently: true });
  }
  if (!_colorCtx) return input;
  try {
    _colorCtx.clearRect(0, 0, 1, 1);
    _colorCtx.fillStyle = input;
    _colorCtx.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = _colorCtx.getImageData(0, 0, 1, 1).data;
    return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
  } catch {
    return input;
  }
}

/// glide-data-grid の配色をアプリのセマンティックトークンへ揃える（ライト/ダーク対応）。
/// oklch トークンを getComputedStyle で読み → resolveColor で rgb/hex に正規化して渡す。
/// テーマ切替（next-themes）で再計算する。編集セルのハイライト色も同時に返す。
function useGlideTheme(): { theme: Partial<Theme>; editedBg: string } {
  const { resolvedTheme } = useTheme();
  const [state, setState] = React.useState<{ theme: Partial<Theme>; editedBg: string }>({
    theme: {},
    editedBg: "rgba(0,0,0,0.05)",
  });

  React.useEffect(() => {
    // クラス反映後に読むため 1 フレーム遅らせる。
    const id = requestAnimationFrame(() => {
      const cs = getComputedStyle(document.documentElement);
      const v = (name: string) => cs.getPropertyValue(name).trim();
      const rc = (name: string) => resolveColor(v(name));
      setState({
        theme: {
          accentColor: rc("--primary"),
          accentLight: rc("--accent"),
          textDark: rc("--foreground"),
          textMedium: rc("--muted-foreground"),
          textLight: rc("--muted-foreground"),
          textBubble: rc("--foreground"),
          bgIconHeader: rc("--muted-foreground"),
          fgIconHeader: rc("--background"),
          textHeader: rc("--muted-foreground"),
          textHeaderSelected: rc("--foreground"),
          bgCell: rc("--card"),
          bgCellMedium: rc("--muted"),
          bgHeader: rc("--muted"),
          bgHeaderHasFocus: rc("--accent"),
          bgHeaderHovered: rc("--accent"),
          bgBubble: rc("--popover"),
          bgSearchResult: rc("--accent"),
          borderColor: rc("--border"),
          drilldownBorder: rc("--border"),
          linkColor: rc("--primary"),
          fontFamily: v("--font-sans") || "ui-sans-serif, system-ui, sans-serif",
          baseFontStyle: "13px",
          headerFontStyle: "600 12px",
        },
        editedBg: resolveColor(`color-mix(in oklab, ${v("--primary")} 12%, ${v("--card")})`),
      });
    });
    return () => cancelAnimationFrame(id);
  }, [resolvedTheme]);

  return state;
}

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
      {/* 選択→AI 指示（Task 11.10）: 範囲選択時にフロートボタンを出す。 */}
      {onAskAi && selectionRange ? (
        <button
          type="button"
          data-testid="csv-ask-ai"
          onClick={() => onAskAi(buildSelectionInfo(selectionRange))}
          className="absolute right-3 top-3 z-10 inline-flex items-center gap-1 rounded-full border border-border/60 bg-card px-3 py-1.5 text-xs font-medium text-primary shadow-sm transition-colors hover:bg-accent"
        >
          <Sparkles className="size-3.5" aria-hidden />
          選択範囲を AI に依頼
        </button>
      ) : null}
      {/* Glide のオーバーレイエディタ用ポータル。 */}
      <div id="portal" style={{ position: "fixed", left: 0, top: 0, zIndex: 9999 }} />
    </div>
  );
});
