"use client";

/// テーブルの作業画面（Task 15.5）: ビュー切替・絞り込み/並べ替え・列の表示・グループ化・書式・
/// スキーマ編集・レコード追加と、仮想化グリッド／カード系ビュー。
///
/// 表示の状態（列の順・見出し・幅・非表示・固定・グループ・書式・条件・並べ替え）は**ビューの版**として
/// 保存する（自分のビューは新しい版を追記・他人のビューは「名前を付けて保存」で自分のものにする）。
/// 行は保存ビューでも毎回 `run_query` で**閲覧者本人の権限**で取る（作成者の権限を引き継がない）。

import {
  AlertTriangle,
  CalendarDays,
  Columns3,
  KanbanSquare,
  LayoutGrid,
  Loader2,
  Plus,
  Save,
  Settings2,
  Table2,
} from "lucide-react";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";

import { CalendarMonth, GalleryGrid, KanbanBoard } from "@/components/data/card-views";
import { DataGrid, type DataGridHandle, type DataGridStatus } from "@/components/data/data-grid";
import { RecordDialog } from "@/components/data/record-dialog";
import { SchemaEditor } from "@/components/data/schema-editor";
import {
  ColumnsControl,
  FilterControl,
  FormatControl,
  GroupControl,
  SortControl,
} from "@/components/data/view-controls";
import { ViewSwitcher } from "@/components/data/view-switcher";
import { usePageChrome } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { toast } from "@/components/ui/use-toast";
import { useMe } from "@/hooks/use-me";
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  DataApiError,
  getView,
  listIndexes,
  promoteIndex,
  type Condition,
  type DataQuery,
  type DataTable,
  type DataViewSummary,
  type GridView,
  type SortKey,
  type TableViewerContext,
  type ViewSpec,
} from "@/lib/data-api";
import {
  buildFilter,
  columnsToConfig,
  defaultSpec,
  effectiveSort,
  resolveColumns,
  splitFilter,
  type ColumnModel,
  type FilterRow,
} from "@/lib/data-view";

type Loaded = {
  viewId: string | null;
  name: string;
  version: number | null;
  owner: string | null;
  spec: ViewSpec;
  query: DataQuery;
};

function countLabel(s: DataGridStatus | null): string {
  if (!s) return "";
  const c = s.count;
  if (c && "exact" in c) return `${c.exact.toLocaleString("ja-JP")} 件`;
  if (c && "at_least" in c) return `${c.at_least.toLocaleString("ja-JP")} 件以上`;
  return s.done ? `${s.loaded.toLocaleString("ja-JP")} 件` : "";
}

export function TableWorkspace({
  table,
  viewer,
  views,
  onTableChanged,
  onViewsChanged,
}: {
  table: DataTable;
  viewer: TableViewerContext;
  views: DataViewSummary[];
  onTableChanged: (t: DataTable) => void;
  onViewsChanged: () => void;
}) {
  const me = useMe();
  const router = useRouter();
  const params = useSearchParams();
  const viewParam = params.get("view");
  const masked = React.useMemo(() => new Set(viewer.masked_fields), [viewer]);
  const gridRef = React.useRef<DataGridHandle>(null);

  const [loaded, setLoaded] = React.useState<Loaded>({
    viewId: null,
    name: "既定のビュー",
    version: null,
    owner: null,
    spec: defaultSpec(),
    query: {},
  });
  const [spec, setSpec] = React.useState<ViewSpec>(defaultSpec());
  const [columns, setColumns] = React.useState<ColumnModel[]>([]);
  const [filterRows, setFilterRows] = React.useState<FilterRow[]>([]);
  const [advanced, setAdvanced] = React.useState<Condition | null>(null);
  const [sort, setSort] = React.useState<SortKey[]>([]);
  const [status, setStatus] = React.useState<DataGridStatus | null>(null);
  const [problem, setProblem] = React.useState<
    | { kind: "preparing"; field: string; progress: number | null }
    | { kind: "approval"; field: string; indexed: number; cap: number }
    | { kind: "error"; message: string }
    | null
  >(null);
  const [schemaOpen, setSchemaOpen] = React.useState(false);
  const [recordOpen, setRecordOpen] = React.useState(false);
  const [viewLoading, setViewLoading] = React.useState(false);

  const grid: GridView | null = spec.kind === "grid" ? spec : null;

  /// 読み込んだビューを画面の状態へ展開する。
  const apply = React.useCallback(
    (l: Loaded) => {
      setLoaded(l);
      setSpec(l.spec);
      setColumns(resolveColumns(table.schema.fields, l.spec.kind === "grid" ? l.spec : null, masked));
      const f = splitFilter(l.query.filter);
      setFilterRows(f.rows);
      setAdvanced(f.advanced);
      setSort(l.query.sort ?? []);
      setProblem(null);
    },
    [table.schema.fields, masked],
  );

  // ビューの切替（URL の ?view= が正）。
  React.useEffect(() => {
    let active = true;
    if (!viewParam) {
      apply({ viewId: null, name: "既定のビュー", version: null, owner: null, spec: defaultSpec(), query: {} });
      return;
    }
    setViewLoading(true);
    getView(viewParam)
      .then((v) => {
        if (!active) return;
        const summary = views.find((x) => x.id === v.id);
        apply({
          viewId: v.id,
          name: summary?.name ?? "ビュー",
          version: v.version,
          owner: summary?.owner ?? null,
          spec: v.display ?? defaultSpec(),
          query: v.query,
        });
      })
      .catch((e) => {
        if (!active) return;
        toast({ variant: "destructive", title: "ビューを開けませんでした", description: e instanceof Error ? e.message : String(e) });
        router.replace(`/data/${table.id}`);
      })
      .finally(() => active && setViewLoading(false));
    return () => {
      active = false;
    };
    // views はビューの名前の解決にだけ使う（一覧の再読込でビューを開き直さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [viewParam, table.id, apply]);

  // スキーマが変わったら列を作り直す（並び・見出し・幅などの今の設定は保つ）。
  React.useEffect(() => {
    setColumns((prev) => {
      const g: GridView = { kind: "grid", columns: columnsToConfig(prev) } as GridView;
      return resolveColumns(table.schema.fields, prev.length ? g : grid, masked);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [table.schema, masked]);

  const visible = React.useMemo(() => columns.filter((c) => !c.hidden), [columns]);

  // 実行するクエリ（表示する列だけを取る・グループ化の列を並べ替えの先頭に）。
  const query: DataQuery = React.useMemo(() => {
    const filter = buildFilter(filterRows, advanced, columns);
    const s = effectiveSort(spec, sort);
    const select = spec.kind === "grid" ? visible.filter((c) => !c.system).map((c) => c.key) : undefined;
    return {
      ...(filter ? { filter } : {}),
      ...(s.length ? { sort: s } : {}),
      ...(select ? { select } : {}),
    };
  }, [filterRows, advanced, columns, spec, sort, visible]);

  // 保存する本文（列の設定はビューごと）。
  const body = React.useMemo(() => {
    const display: ViewSpec =
      spec.kind === "grid" ? { ...spec, columns: columnsToConfig(columns) } : spec;
    const filter = buildFilter(filterRows, advanced, columns);
    const q: DataQuery = { ...(filter ? { filter } : {}), ...(sort.length ? { sort } : {}) };
    return { table_id: table.id, query: q, display };
  }, [spec, columns, filterRows, advanced, sort, table.id]);

  const savedBody = React.useMemo(() => {
    const display: ViewSpec =
      loaded.spec.kind === "grid"
        ? {
            ...loaded.spec,
            columns: columnsToConfig(
              resolveColumns(table.schema.fields, loaded.spec, masked),
            ),
          }
        : loaded.spec;
    const f = splitFilter(loaded.query.filter);
    const filter = buildFilter(f.rows, f.advanced, columns);
    const q: DataQuery = {
      ...(filter ? { filter } : {}),
      ...(loaded.query.sort?.length ? { sort: loaded.query.sort } : {}),
    };
    return { table_id: table.id, query: q, display };
  }, [loaded, table, masked, columns]);
  const dirty = JSON.stringify(body) !== JSON.stringify(savedBody);
  const mine = loaded.viewId !== null && loaded.owner === me.data?.id;

  // 並べ替えの矢印（代替列で並べている列にも出す）。
  const sortState = React.useMemo(() => {
    const m = new Map<string, "asc" | "desc">();
    for (const k of effectiveSort(spec, sort)) {
      for (const c of columns) {
        if ((c.sortBy ?? c.key) === k.field && !m.has(c.key)) m.set(c.key, k.descending ? "desc" : "asc");
      }
    }
    return m;
  }, [spec, sort, columns]);

  const onHeaderClick = React.useCallback(
    (key: string) => {
      const c = columns.find((x) => x.key === key);
      if (!c) return;
      const field = c.sortBy ?? c.key;
      setSort((s) => {
        const cur = s.find((k) => k.field === field);
        // 昇順 → 降順 → 解除。
        if (!cur) return [{ field, descending: false }];
        if (!cur.descending) return [{ field, descending: true }];
        return [];
      });
    },
    [columns],
  );

  const onColumnResize = React.useCallback((key: string, width: number) => {
    setColumns((cs) => cs.map((c) => (c.key === key ? { ...c, width: Math.round(width) } : c)));
  }, []);

  const onColumnMove = React.useCallback((fromKey: string, toKey: string) => {
    setColumns((cs) => {
      const from = cs.findIndex((c) => c.key === fromKey);
      const to = cs.findIndex((c) => c.key === toKey);
      if (from < 0 || to < 0) return cs;
      const next = [...cs];
      const [m] = next.splice(from, 1);
      next.splice(to, 0, m);
      return next;
    });
  }, []);

  const onQueryError = React.useCallback((e: Error) => {
    if (e instanceof DataApiError) {
      const p = e.indexPreparing;
      if (p) return setProblem({ kind: "preparing", ...p });
      const a = e.approvalRequired;
      if (a) return setProblem({ kind: "approval", ...a });
    }
    setProblem({ kind: "error", message: e.message });
  }, []);

  // 条件・並べ替えを変えたら前の案内は捨てる（新しいクエリの結果で改めて決まる）。
  const queryKey = JSON.stringify(query);
  React.useEffect(() => setProblem(null), [queryKey]);

  // 索引の準備中は進捗を読み直し、整ったら読み直す。
  React.useEffect(() => {
    if (problem?.kind !== "preparing") return;
    const t = window.setInterval(async () => {
      try {
        const o = await listIndexes(table.id);
        const st = o.fields.find((f) => f.field === problem.field)?.status;
        if (st === "active") {
          setProblem(null);
          gridRef.current?.reload();
        } else {
          setProblem({ ...problem, progress: o.backfill_progress ?? problem.progress });
        }
      } catch {
        // 次の周期で再試行する。
      }
    }, 2000);
    return () => window.clearInterval(t);
  }, [problem, table.id]);

  const approve = async () => {
    if (problem?.kind !== "approval") return;
    try {
      const r = await promoteIndex(table.id, problem.field, true);
      setProblem(r.status === "active" ? null : { kind: "preparing", field: problem.field, progress: r.progress ?? null });
      if (r.status === "active") gridRef.current?.reload();
    } catch (e) {
      toast({ variant: "destructive", title: "索引を作れませんでした", description: e instanceof Error ? e.message : String(e) });
    }
  };

  const switchKind = (kind: ViewSpec["kind"]) => {
    if (kind === spec.kind) return;
    const selectCol = columns.find((c) => c.field?.type === "select");
    const dateCol = columns.find((c) => c.field?.type === "date" || c.field?.type === "date_time");
    const firstText = columns.find((c) => !c.system)?.key;
    const cards = columns.filter((c) => !c.system && !c.hidden).slice(0, 4).map((c) => c.key);
    if (kind === "grid") setSpec({ kind: "grid", columns: columnsToConfig(columns), row_height: grid?.row_height ?? "short" });
    if (kind === "kanban" && selectCol) setSpec({ kind: "kanban", group_field: selectCol.key, card_fields: cards.filter((k) => k !== selectCol.key) });
    if (kind === "gallery") setSpec({ kind: "gallery", title_field: firstText, card_fields: cards.slice(1) });
    if (kind === "calendar") setSpec({ kind: "calendar", date_field: dateCol?.key ?? "_created_at", title_field: firstText });
  };
  const canKanban = columns.some((c) => c.field?.type === "select");
  const narrow = useMediaQuery("(max-width: 639.98px)");
  const kindOptions = [
    { value: "grid", label: "表", icon: Table2, testId: "view-kind-grid" },
    // カンバンは単一選択の列が要る（無いテーブルでは出さない）。
    ...(canKanban ? [{ value: "kanban", label: "カンバン", icon: KanbanSquare }] : []),
    { value: "gallery", label: "カード", icon: LayoutGrid },
    { value: "calendar", label: "カレンダー", icon: CalendarDays },
  ];

  usePageChrome(
    () => ({
      title: table.name,
      subtitle: loaded.viewId ? loaded.name : undefined,
      back: { href: "/data", label: "テーブル一覧へ" },
    }),
    [table.name, loaded.viewId, loaded.name],
  );

  const err = (e: Error) => onQueryError(e);
  const labelOf = (key: string) => columns.find((c) => c.key === key)?.label ?? key;
  // 索引の準備中・承認待ちは失敗ではなく「待ち」。グリッドは直前の行を薄く残して進捗を出す。
  const waiting =
    problem?.kind === "preparing"
      ? { message: `「${labelOf(problem.field)}」で並べ替え・絞り込みできるよう準備中です`, progress: problem.progress }
      : problem?.kind === "approval"
        ? { message: `「${labelOf(problem.field)}」の索引の承認を待っています`, progress: null }
        : null;

  return (
    <div className="flex h-full min-h-0 flex-col gap-2 px-3 pb-3 pt-2 md:px-5 md:pt-4">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1.5">
        <ViewSwitcher
          table={table}
          views={views}
          current={loaded}
          mine={mine}
          dirty={dirty}
          body={body}
          onOpen={(id) => router.push(id ? `/data/${table.id}?view=${id}` : `/data/${table.id}`)}
          onSaved={(id, version, name) => {
            onViewsChanged();
            setLoaded((l) => ({ ...l, viewId: id, version, name, owner: me.data?.id ?? l.owner, spec: body.display, query: body.query }));
            if (id !== loaded.viewId) router.replace(`/data/${table.id}?view=${id}`);
          }}
        />
        {viewLoading ? <Loader2 className="size-4 animate-spin text-muted-foreground" aria-label="ビューを読み込み中" /> : null}
        <div className="mx-0.5 hidden h-5 w-px bg-border sm:block" aria-hidden />
        <div className="flex items-center gap-0.5 overflow-x-auto scrollbar-none max-sm:-mx-1 max-sm:w-full max-sm:px-1">
          <FilterControl
            columns={columns}
            rows={filterRows}
            onChange={setFilterRows}
            advanced={advanced !== null}
            onClearAdvanced={() => setAdvanced(null)}
          />
          <SortControl columns={columns} sort={sort} maxKeys={4 - (grid?.group_by?.length ?? 0)} onChange={setSort} />
          {spec.kind === "grid" ? (
            <>
              <ColumnsControl columns={columns} onChange={setColumns} />
              <GroupControl
                columns={columns}
                groups={grid?.group_by ?? []}
                onChange={(group_by) => setSpec({ ...(spec as GridView), kind: "grid", group_by } as ViewSpec)}
              />
              <FormatControl
                columns={columns}
                formats={grid?.conditional_format ?? []}
                onChange={(conditional_format) =>
                  setSpec({ ...(spec as GridView), kind: "grid", conditional_format } as ViewSpec)
                }
              />
            </>
          ) : null}
          {viewer.can_manage ? (
            <Button
              variant="ghost"
              size="sm"
              className="h-8 gap-1.5 px-2.5 text-[13px]"
              onClick={() => setSchemaOpen(true)}
              data-testid="schema-editor-open"
            >
              <Settings2 className="size-4" aria-hidden />
              <span className="max-sm:sr-only">列の編集</span>
            </Button>
          ) : null}
        </div>
        <div className="ml-auto flex items-center gap-2 max-sm:w-full max-sm:justify-end">
          <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground" data-testid="data-count" aria-live="polite">
            {countLabel(status)}
          </span>
          {narrow ? (
            // 狭い画面はセグメントの文字が折り返すので、選択式にする。
            <Select value={spec.kind} onValueChange={(v) => switchKind(v as ViewSpec["kind"])}>
              <SelectTrigger aria-label="表示形式" className="h-8 w-32 text-[13px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {kindOptions.map((o) => (
                  <SelectItem key={o.value} value={o.value}>
                    <span className="inline-flex items-center gap-1.5">
                      <o.icon className="size-3.5" aria-hidden />
                      {o.label}
                    </span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : (
            <SegmentedControl
              aria-label="表示形式"
              size="sm"
              value={spec.kind}
              onValueChange={(v) => switchKind(v as ViewSpec["kind"])}
              options={kindOptions}
            />
          )}
          {viewer.can_edit ? (
            <Button size="sm" className="h-8" onClick={() => setRecordOpen(true)} data-testid="add-record">
              <Plus className="size-4" aria-hidden />
              <span className="max-sm:sr-only">レコード</span>
            </Button>
          ) : null}
        </div>
      </div>

      {problem && !(problem.kind === "preparing" && spec.kind === "grid") ? (
        <div
          role="status"
          className="flex items-center gap-2 rounded-lg border px-3 py-2 text-xs"
          style={{
            borderColor: "color-mix(in oklab, var(--season-autumn) 40%, transparent)",
            backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
          }}
          data-testid="data-problem"
        >
          {problem.kind === "preparing" ? (
            <>
              <Loader2 className="size-4 shrink-0 animate-spin" aria-hidden />
              <span className="flex-1">
                「{columns.find((c) => c.key === problem.field)?.label ?? problem.field}」で並べ替え・絞り込みできるよう準備しています
                {problem.progress != null ? `（${Math.round(problem.progress * 100)}%）` : ""}。完了すると自動で表示します。
              </span>
            </>
          ) : problem.kind === "approval" ? (
            <>
              <AlertTriangle className="size-4 shrink-0" aria-hidden />
              <span className="flex-1">
                「{columns.find((c) => c.key === problem.field)?.label ?? problem.field}」で並べ替え・絞り込みするには索引が要ります（索引 {problem.indexed} / 上限 {problem.cap} 列）。
                {viewer.can_manage ? "" : " テーブルの管理者に依頼してください。"}
              </span>
              {viewer.can_manage ? (
                <Button size="sm" variant="outline" className="h-7 text-xs" onClick={approve}>
                  承認して索引を作る
                </Button>
              ) : null}
            </>
          ) : (
            <>
              <AlertTriangle className="size-4 shrink-0" aria-hidden />
              <span className="flex-1">{problem.message}</span>
              <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={() => setProblem(null)}>
                閉じる
              </Button>
            </>
          )}
        </div>
      ) : null}

      <div className="min-h-0 flex-1">
        {spec.kind === "grid" ? (
          visible.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-3 rounded-lg border border-dashed text-sm text-muted-foreground">
              <Columns3 className="size-6" aria-hidden />
              表示する列がありません
            </div>
          ) : (
            <DataGrid
              ref={gridRef}
              tableId={table.id}
              columns={visible}
              query={query}
              grid={grid}
              canEdit={viewer.can_edit}
              sortState={sortState}
              onHeaderClick={onHeaderClick}
              onColumnResize={onColumnResize}
              onColumnMove={onColumnMove}
              onQueryError={onQueryError}
              onStatus={setStatus}
              waiting={waiting}
            />
          )
        ) : spec.kind === "kanban" ? (
          <KanbanBoard tableId={table.id} spec={spec} columns={columns} query={query} onError={err} />
        ) : spec.kind === "gallery" ? (
          <GalleryGrid tableId={table.id} spec={spec} columns={columns} query={query} onError={err} />
        ) : (
          <CalendarMonth tableId={table.id} spec={spec} columns={columns} query={query} onError={err} />
        )}
      </div>

      {dirty && loaded.viewId && !mine ? (
        <p className="text-[11px] text-muted-foreground">
          <Save className="mr-1 inline size-3" aria-hidden />
          このビューは他の人のものです。変更は「名前を付けて保存」で自分のビューにできます（元のビューは変わりません）。
        </p>
      ) : null}

      <SchemaEditor open={schemaOpen} onOpenChange={setSchemaOpen} table={table} onSaved={onTableChanged} />
      <RecordDialog
        open={recordOpen}
        onOpenChange={setRecordOpen}
        table={table}
        masked={masked}
        onCreated={() => gridRef.current?.reload()}
      />
    </div>
  );
}
