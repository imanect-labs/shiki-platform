"use client";

import * as React from "react";
import { AlertCircle, ChevronRight, Download, Loader2, ScrollText, ShieldCheck } from "lucide-react";

import { cn } from "@/lib/utils";
import { auditExportUrl, fetchAuditEvents, type AuditEvent, type AuditQuery } from "@/lib/audit-api";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { AdminPage, Panel } from "../admin-page";
import { formatInt } from "../format";
import { AuditDetail, Verdict, actorLabel } from "./audit-detail";
import { AuditFilters } from "./audit-filters";
import { formatTime, useAuditQuery } from "./audit-query";

type State = {
  events: AuditEvent[];
  next: string | null;
  loading: boolean;
  more: boolean;
  error: string | null;
  /// 「さらに読み込む」だけの失敗（読み込み済みの一覧は残す）。
  moreError: string | null;
};

/// 条件が変わるたびに 1 ページ目を読み直し、「さらに読み込む」で続きを足す。
function useAuditEvents(query: AuditQuery) {
  const [state, setState] = React.useState<State>({
    events: [],
    next: null,
    loading: true,
    more: false,
    error: null,
    moreError: null,
  });
  const key = JSON.stringify(query);
  const seq = React.useRef(0);
  const load = React.useCallback(() => {
    const id = ++seq.current;
    setState((s) => ({ ...s, loading: true, error: null }));
    fetchAuditEvents(query)
      .then((r) => {
        if (id !== seq.current) return;
        setState({
          events: r.events,
          next: r.next_cursor ?? null,
          loading: false,
          more: false,
          error: null,
          moreError: null,
        });
      })
      .catch((e: unknown) => {
        if (id !== seq.current) return;
        setState({
          events: [],
          next: null,
          loading: false,
          more: false,
          error: e instanceof Error ? e.message : String(e),
          moreError: null,
        });
      });
    // key が query の内容を表す（オブジェクトの同一性では読み直さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  React.useEffect(load, [load]);
  const loadMore = React.useCallback(() => {
    if (!state.next || state.more) return;
    const id = seq.current;
    setState((s) => ({ ...s, more: true, moreError: null }));
    fetchAuditEvents(query, state.next)
      .then((r) => {
        if (id !== seq.current) return;
        setState((s) => ({
          ...s,
          events: [...s.events, ...r.events],
          next: r.next_cursor ?? null,
          more: false,
        }));
      })
      .catch((e: unknown) => {
        if (id !== seq.current) return;
        setState((s) => ({ ...s, more: false, moreError: e instanceof Error ? e.message : String(e) }));
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, state.next, state.more]);
  return { ...state, reload: load, loadMore };
}

function ExportMenu({ query }: { query: AuditQuery }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm">
          <Download aria-hidden />
          書き出す
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
          いまの条件で最大 10 万件。書き出したことも監査ログに残ります。
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <a href={auditExportUrl(query, "csv")} download>
            CSV（Excel で開ける）
          </a>
        </DropdownMenuItem>
        <DropdownMenuItem asChild>
          <a href={auditExportUrl(query, "jsonl")} download>
            JSONL（1 行 1 件）
          </a>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ListSkeleton() {
  return (
    <div className="flex flex-col gap-2 p-4" aria-busy="true" aria-label="読み込み中">
      {Array.from({ length: 8 }, (_, i) => (
        <Skeleton key={i} className="h-7 w-full" />
      ))}
    </div>
  );
}

function MoreButton({
  next,
  more,
  error,
  onClick,
}: {
  next: string | null;
  more: boolean;
  error: string | null;
  onClick: () => void;
}) {
  if (!next) return null;
  return (
    <div className="flex flex-col items-center gap-1 border-t p-2.5">
      {error ? (
        <p role="alert" className="text-xs text-destructive">
          続きを読み込めませんでした: {error}
        </p>
      ) : null}
      <Button variant="ghost" size="sm" onClick={onClick} disabled={more}>
        {more ? <Loader2 className="animate-spin" aria-hidden /> : null}
        {error ? "もう一度読み込む" : "さらに読み込む"}
      </Button>
    </div>
  );
}

/// 監査ログ（#634）。PC は表＋右の詳細、スマホはカードの一覧＋タップで下からのシート。
/// 監査閲覧者（`organization#audit_viewer`）だけが開ける。読んだこと自体も監査に残る。
export function AuditView() {
  const { query, setQuery, patch } = useAuditQuery();
  const { events, next, loading, more, error, moreError, reload, loadMore } = useAuditEvents(query);
  const [selectedId, setSelectedId] = React.useState<number | null>(null);
  const [sheetOpen, setSheetOpen] = React.useState(false);
  const selected = events.find((e) => e.id === selectedId) ?? events[0] ?? null;
  const narrow = (p: Partial<AuditQuery>) => {
    setSheetOpen(false);
    patch(p);
  };

  const summary = loading
    ? "読み込み中…"
    : `${formatInt(events.length)} 件${next ? "（さらにあります）" : ""}`;

  let body: React.ReactNode;
  if (error) {
    body = (
      <EmptyState
        icon={AlertCircle}
        title="監査ログを読み込めませんでした"
        description={error}
        action={
          <Button variant="outline" size="sm" onClick={reload}>
            もう一度読み込む
          </Button>
        }
      />
    );
  } else if (!loading && events.length === 0) {
    body = (
      <EmptyState
        icon={ScrollText}
        title="この条件に合う記録はありません"
        description="期間を広げるか、条件を減らしてください。"
      />
    );
  } else {
    body = (
      <>
        <div className="md:hidden">
          {loading ? (
            <Panel>
              <ListSkeleton />
            </Panel>
          ) : (
            <div className="rule-soft overflow-hidden rounded-xl border bg-card">
              <ul aria-label="監査ログ">
                {events.map((e, i) => (
                  <li key={e.id} className={cn(i > 0 && "shiki-dash-top")}>
                    <button
                      type="button"
                      onClick={() => {
                        setSelectedId(e.id);
                        setSheetOpen(true);
                      }}
                      className="flex min-h-[64px] w-full items-center gap-3 px-4 py-3 text-left outline-none transition-colors active:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
                    >
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-2">
                          <Verdict decision={e.decision} />
                          <span className="truncate text-[15px] font-medium">{actorLabel(e)}</span>
                        </span>
                        <span className="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
                          <span className="min-w-0 truncate font-mono">{e.action}</span>
                          <span className="ml-auto shrink-0 font-mono tabular-nums">
                            {formatTime(e.created_at).slice(5, 16)}
                          </span>
                        </span>
                      </span>
                      <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                    </button>
                  </li>
                ))}
              </ul>
              <MoreButton next={next} more={more} error={moreError} onClick={loadMore} />
            </div>
          )}
        </div>
        <Sheet open={sheetOpen && selected !== null} onOpenChange={setSheetOpen}>
          <SheetContent side="bottom" className="md:hidden">
            {selected ? (
              <>
                <SheetHeader className="pr-14">
                  <SheetTitle className="break-all font-mono">{selected.action}</SheetTitle>
                  <SheetDescription>
                    {selected.decision === "allow" ? "許可された操作" : "拒否された操作"}
                  </SheetDescription>
                </SheetHeader>
                <SheetBody>
                  <AuditDetail event={selected} onNarrow={narrow} />
                </SheetBody>
              </>
            ) : null}
          </SheetContent>
        </Sheet>
        <div className="grid gap-4 max-md:hidden lg:grid-cols-[minmax(0,1fr)_360px]">
          <Panel className="min-w-0">
            {loading ? (
              <ListSkeleton />
            ) : (
              <>
                <div className="overflow-x-auto">
                  <table className="w-full min-w-[680px] border-collapse text-[12.5px]">
                    <caption className="sr-only">監査ログ（新しい順）</caption>
                    <thead>
                      <tr className="bg-muted/40 text-left text-xs text-muted-foreground">
                        <th scope="col" className="px-4 py-2 font-medium">時刻</th>
                        <th scope="col" className="px-2 py-2 font-medium">判定</th>
                        <th scope="col" className="px-2 py-2 font-medium">誰が</th>
                        <th scope="col" className="px-2 py-2 font-medium">操作</th>
                        <th scope="col" className="px-4 py-2 font-medium">対象</th>
                      </tr>
                    </thead>
                    <tbody>
                      {events.map((e) => {
                        const current = selected?.id === e.id;
                        return (
                          <tr
                            key={e.id}
                            onClick={() => setSelectedId(e.id)}
                            className={cn(
                              "cursor-pointer border-t",
                              current ? "bg-accent/60" : "hover:bg-accent/30",
                            )}
                          >
                            <td className="whitespace-nowrap px-4 py-2 font-mono tabular-nums text-muted-foreground">
                              <button
                                type="button"
                                onClick={() => setSelectedId(e.id)}
                                aria-label={`${formatTime(e.created_at)} の記録を見る`}
                                aria-current={current ? "true" : undefined}
                                className="outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring"
                              >
                                {formatTime(e.created_at)}
                              </button>
                            </td>
                            <td className="px-2 py-2">
                              <Verdict decision={e.decision} />
                            </td>
                            <td className="max-w-[12rem] truncate px-2 py-2" title={e.actor}>
                              {actorLabel(e)}
                            </td>
                            <td className="px-2 py-2 font-mono">{e.action}</td>
                            <td
                              className="max-w-[16rem] truncate px-4 py-2 font-mono text-muted-foreground"
                              title={`${e.object_type}:${e.object_id}`}
                            >
                              {e.object_type}:{e.object_id}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
                <MoreButton next={next} more={more} error={moreError} onClick={loadMore} />
              </>
            )}
          </Panel>
          <div className="min-w-0 lg:sticky lg:top-4 lg:self-start">
            <Panel title={selected ? <span className="break-all font-mono">{selected.action}</span> : "詳細"}>
              <div className="px-4 py-3">
                {selected ? (
                  <AuditDetail event={selected} onNarrow={narrow} />
                ) : (
                  <Skeleton className="h-40 w-full" />
                )}
              </div>
            </Panel>
          </div>
        </div>
      </>
    );
  }

  return (
    <AdminPage
      title="監査ログ"
      description={
        <span className="inline-flex items-center gap-1.5">
          <ShieldCheck className="size-3.5 shrink-0" aria-hidden />
          操作・権限判定・引用チャンクの記録です。ここで検索・書き出ししたことも記録に残ります。
        </span>
      }
      actions={<ExportMenu query={query} />}
      wide
    >
      <AuditFilters query={query} onApply={setQuery} />
      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground" aria-live="polite">
        <span className="tabular-nums">
          {query.from} 〜 {query.to}・{summary}
        </span>
      </div>
      {body}
    </AdminPage>
  );
}
