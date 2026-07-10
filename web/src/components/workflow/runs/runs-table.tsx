"use client";

/// 実行履歴の data-table（Task 10.14・openstatus data-table 風）。
///
/// フィルタは server-side（status/trigger_kind を query に載せる）・ページングは
/// keyset（created_at, run_id）で「さらに読み込む」。表示中は先頭ページを 5 秒ごとに
/// 再取得して既知行の状態を更新し、新しい run を先頭に差し込む（古いページは
/// terminal 済みがほとんどなので触らない）。

import * as React from "react";
import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { Check, ChevronRight, Filter, ListX, Loader2, RefreshCw } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { listRuns, type RunListItem } from "@/lib/workflow-run-api";
import {
  formatDateTime,
  formatDuration,
  RUN_STATUS_OPTIONS,
  RunStatusBadge,
  TRIGGER_KIND_LABELS,
  TRIGGER_KIND_OPTIONS,
} from "./status";

const PAGE_SIZE = 30;
const REFRESH_INTERVAL_MS = 5000;

/// 状態/きっかけの複数選択フィルタ（openstatus 風の faceted filter）。
function FacetFilter({
  label,
  options,
  selected,
  onChange,
}: {
  label: string;
  options: { value: string; label: string }[];
  selected: string[];
  onChange: (next: string[]) => void;
}) {
  const toggle = (value: string) =>
    onChange(
      selected.includes(value)
        ? selected.filter((v) => v !== value)
        : [...selected, value],
    );
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm" className="h-8 border-dashed text-xs">
          <Filter className="size-3.5" aria-hidden />
          {label}
          {selected.length > 0 ? (
            <Badge variant="secondary" className="px-1.5 py-0 text-[10px]">
              {selected.length}
            </Badge>
          ) : null}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-44 p-1">
        {options.map((o) => {
          const active = selected.includes(o.value);
          return (
            <button
              key={o.value}
              type="button"
              onClick={() => toggle(o.value)}
              className={cn(
                "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs",
                "transition-colors duration-fast hover:bg-accent",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              )}
            >
              <span
                className={cn(
                  "flex size-3.5 items-center justify-center rounded border",
                  active ? "border-primary bg-primary text-primary-foreground" : "bg-background",
                )}
              >
                {active ? <Check className="size-2.5" aria-hidden /> : null}
              </span>
              {o.label}
            </button>
          );
        })}
        {selected.length > 0 ? (
          <button
            type="button"
            onClick={() => onChange([])}
            className="mt-1 w-full rounded-md border-t px-2 py-1.5 text-center text-[11px] text-muted-foreground transition-colors duration-fast hover:bg-accent"
          >
            選択を解除
          </button>
        ) : null}
      </PopoverContent>
    </Popover>
  );
}

const COLUMNS: ColumnDef<RunListItem>[] = [
  {
    id: "status",
    header: "状態",
    cell: ({ row }) => <RunStatusBadge status={row.original.status} />,
  },
  {
    id: "trigger",
    header: "きっかけ",
    cell: ({ row }) => (
      <span className="text-xs">
        {TRIGGER_KIND_LABELS[row.original.triggerKind] ?? row.original.triggerKind}
      </span>
    ),
  },
  {
    id: "version",
    header: "バージョン",
    cell: ({ row }) => (
      <span className="text-xs text-muted-foreground">v{row.original.version}</span>
    ),
  },
  {
    id: "createdAt",
    header: "受付日時",
    cell: ({ row }) => (
      <span className="text-xs tabular-nums">
        {formatDateTime(row.original.createdAt)}
      </span>
    ),
  },
  {
    id: "duration",
    header: "所要時間",
    cell: ({ row }) => (
      <span className="text-xs tabular-nums text-muted-foreground">
        {formatDuration(row.original.startedAt, row.original.finishedAt)}
      </span>
    ),
  },
  {
    id: "runId",
    header: "実行 ID",
    cell: ({ row }) => (
      <span className="font-mono text-[11px] text-muted-foreground">
        {row.original.runId.slice(0, 8)}
      </span>
    ),
  },
  {
    id: "open",
    header: "",
    cell: () => (
      <ChevronRight className="size-4 text-muted-foreground" aria-hidden />
    ),
  },
];

export function RunsTable({
  workflowId,
  activeRunId,
  onOpenRun,
  refreshSignal,
}: {
  workflowId: string;
  activeRunId: string | null;
  onOpenRun: (runId: string) => void;
  /// 親（詳細シートの cancel/retry 等）が bump すると先頭ページを取り直す。
  refreshSignal: number;
}) {
  const [items, setItems] = React.useState<RunListItem[] | null>(null);
  const [statuses, setStatuses] = React.useState<string[]>([]);
  const [triggerKinds, setTriggerKinds] = React.useState<string[]>([]);
  const [exhausted, setExhausted] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);

  const filter = React.useMemo(
    () => ({ statuses, triggerKinds, limit: PAGE_SIZE }),
    [statuses, triggerKinds],
  );

  /// 先頭ページ取得。merge=true なら既知行を更新し新規行を先頭に差し込む。
  const loadFirst = React.useCallback(
    async (merge: boolean) => {
      const fresh = await listRuns(workflowId, filter);
      setItems((prev) => {
        if (!merge || prev === null) {
          setExhausted(fresh.length < PAGE_SIZE);
          return fresh;
        }
        const freshById = new Map(fresh.map((i) => [i.runId, i]));
        const known = new Set(prev.map((i) => i.runId));
        // 先頭ページの窓（fresh が満杯なら最古行以降）に入るのに fresh に無い既知行は、
        // フィルタに合致しなくなった行（例: running 絞り込み中に完了）なので落とす。
        const cutoff = fresh.length === PAGE_SIZE ? fresh[fresh.length - 1].createdAt : null;
        const updated = prev
          .filter(
            (i) =>
              freshById.has(i.runId) || (cutoff !== null && i.createdAt < cutoff),
          )
          .map((i) => freshById.get(i.runId) ?? i);
        const newOnes = fresh.filter((i) => !known.has(i.runId));
        return [...newOnes, ...updated];
      });
    },
    [workflowId, filter],
  );

  // フィルタ変更・明示リフレッシュで取り直し。
  React.useEffect(() => {
    setItems(null);
    loadFirst(false).catch(() => setItems([]));
  }, [loadFirst]);
  React.useEffect(() => {
    if (refreshSignal > 0) loadFirst(true).catch(() => undefined);
  }, [refreshSignal, loadFirst]);

  // 表示中の自動更新（タブが裏にある間は止める）。
  React.useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") {
        loadFirst(true).catch(() => undefined);
      }
    }, REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [loadFirst]);

  const loadMore = async () => {
    if (!items || items.length === 0) return;
    setLoadingMore(true);
    try {
      const last = items[items.length - 1];
      const page = await listRuns(workflowId, {
        ...filter,
        before: { createdAt: last.createdAt, runId: last.runId },
      });
      setItems((prev) => [...(prev ?? []), ...page]);
      if (page.length < PAGE_SIZE) setExhausted(true);
    } catch {
      // 次回のクリックで再試行できる。
    } finally {
      setLoadingMore(false);
    }
  };

  const table = useReactTable({
    data: items ?? [],
    columns: COLUMNS,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.runId,
  });

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <FacetFilter
          label="状態"
          options={RUN_STATUS_OPTIONS}
          selected={statuses}
          onChange={setStatuses}
        />
        <FacetFilter
          label="きっかけ"
          options={TRIGGER_KIND_OPTIONS}
          selected={triggerKinds}
          onChange={setTriggerKinds}
        />
        <div className="ml-auto">
          <Button
            variant="ghost"
            size="sm"
            className="h-8 text-xs"
            onClick={() => loadFirst(true).catch(() => undefined)}
          >
            <RefreshCw className="size-3.5" aria-hidden />
            更新
          </Button>
        </div>
      </div>

      {items === null ? (
        <div className="flex items-center justify-center gap-2 rounded-xl border py-16 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          読み込み中…
        </div>
      ) : items.length === 0 ? (
        <div className="rounded-xl border">
          <EmptyState
            icon={ListX}
            title="実行の記録がありません"
            description={
              statuses.length || triggerKinds.length
                ? "フィルタに一致する実行がありません。条件を変えてみてください。"
                : "エディタの「実行」ボタンやスケジュールで動くと、ここに記録が並びます。"
            }
          />
        </div>
      ) : (
        <div className="overflow-x-auto rounded-xl border bg-card">
          <table className="w-full text-left">
            <thead>
              {table.getHeaderGroups().map((hg) => (
                <tr key={hg.id} className="border-b bg-muted/40">
                  {hg.headers.map((h) => (
                    <th
                      key={h.id}
                      className="px-3 py-2 text-[11px] font-medium text-muted-foreground"
                    >
                      {flexRender(h.column.columnDef.header, h.getContext())}
                    </th>
                  ))}
                </tr>
              ))}
            </thead>
            <tbody>
              {table.getRowModel().rows.map((row) => (
                <tr
                  key={row.id}
                  onClick={() => onOpenRun(row.original.runId)}
                  className={cn(
                    "cursor-pointer border-b last:border-b-0",
                    "transition-colors duration-fast hover:bg-accent/50",
                    activeRunId === row.original.runId && "bg-accent/60",
                  )}
                >
                  {row.getVisibleCells().map((cell) => (
                    <td key={cell.id} className="px-3 py-2.5 align-middle">
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
          {!exhausted ? (
            <div className="border-t p-2 text-center">
              <Button
                variant="ghost"
                size="sm"
                className="h-8 text-xs"
                onClick={loadMore}
                disabled={loadingMore}
              >
                {loadingMore ? (
                  <Loader2 className="size-3.5 animate-spin" aria-hidden />
                ) : null}
                さらに読み込む
              </Button>
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}
