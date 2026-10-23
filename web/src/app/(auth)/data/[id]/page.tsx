"use client";

/// テーブルの作業画面（Task 15.5）。テーブル・閲覧者の文脈・保存ビューの一覧を読んでから描く。

import { Database, Loader2 } from "lucide-react";
import { use } from "react";
import * as React from "react";

import { TableWorkspace } from "@/components/data/table-workspace";
import { usePageChrome } from "@/components/shell/page-header-context";
import { EmptyState } from "@/components/ui/empty-state";
import {
  getTable,
  getViewerContext,
  listViews,
  type DataTable,
  type DataViewSummary,
  type TableViewerContext,
} from "@/lib/data-api";

function Pending({ error }: { error: string | null }) {
  usePageChrome(() => ({ back: { href: "/data", label: "テーブル一覧へ" } }), []);
  if (!error) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground" role="status">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        読み込み中…
      </div>
    );
  }
  return (
    <div className="flex h-full items-center justify-center p-8">
      <EmptyState icon={Database} title="テーブルを開けません" description={error} />
    </div>
  );
}

export default function DataTablePage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = use(params);
  const [table, setTable] = React.useState<DataTable | null>(null);
  const [viewer, setViewer] = React.useState<TableViewerContext | null>(null);
  const [views, setViews] = React.useState<DataViewSummary[]>([]);
  const [error, setError] = React.useState<string | null>(null);

  const reloadViews = React.useCallback(() => {
    listViews(id)
      .then(setViews)
      .catch(() => undefined);
  }, [id]);

  React.useEffect(() => {
    let active = true;
    setError(null);
    Promise.all([getTable(id), getViewerContext(id), listViews(id)])
      .then(([t, v, vs]) => {
        if (!active) return;
        setTable(t);
        setViewer(v);
        setViews(vs);
      })
      .catch((e) => active && setError(e instanceof Error ? e.message : "読み込みに失敗しました"));
    return () => {
      active = false;
    };
  }, [id]);

  if (!table || !viewer) return <Pending error={error} />;
  return (
    <React.Suspense fallback={<Pending error={null} />}>
      <TableWorkspace
        table={table}
        viewer={viewer}
        views={views}
        onTableChanged={setTable}
        onViewsChanged={reloadViews}
      />
    </React.Suspense>
  );
}
