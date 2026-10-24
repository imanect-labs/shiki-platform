"use client";

/// カード系のビュー（Task 15.5）: カンバン・ギャラリー・カレンダー。
///
/// 行の取得はグリッドと同じ `run_query`（閲覧者本人の権限）で、keyset の「さらに読み込む」で続きを取る。
/// カレンダーは表示中の月の範囲で絞って取る（日付の列は索引が要る・未索引ならサーバが自動で昇格する）。

import { ChevronLeft, ChevronRight, Loader2 } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { useMediaQuery } from "@/hooks/use-media-query";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import {
  runQuery,
  type Condition,
  type DataQuery,
  type DataRecord,
  type ViewSpec,
} from "@/lib/data-api";
import { cellValue, displayCell, formatValue, roleRefIds, userRefIds, type ColumnModel } from "@/lib/data-view";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { cn } from "@/lib/utils";

type Loaded = { items: DataRecord[]; cursor: string | null; loading: boolean; error: string | null };

function useRecords(tableId: string, query: DataQuery, onError: (e: Error) => void) {
  const [state, setState] = React.useState<Loaded>({ items: [], cursor: null, loading: true, error: null });
  const key = JSON.stringify(query);
  const gen = React.useRef(0);
  const load = React.useCallback(
    async (cursor: string | null) => {
      const g = gen.current;
      setState((s) => ({ ...s, loading: true }));
      try {
        const q = JSON.parse(key) as DataQuery;
        const res = await runQuery(tableId, { ...q, limit: 200, cursor: cursor ?? undefined });
        if (g !== gen.current) return;
        setState((s) => ({
          items: cursor ? [...s.items, ...(res.items ?? [])] : (res.items ?? []),
          cursor: res.next_cursor ?? null,
          loading: false,
          error: null,
        }));
      } catch (e) {
        if (g !== gen.current) return;
        const err = e instanceof Error ? e : new Error(String(e));
        setState((s) => ({ ...s, loading: false, error: err.message }));
        onError(err);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tableId, key],
  );
  React.useEffect(() => {
    gen.current += 1;
    setState({ items: [], cursor: null, loading: true, error: null });
    void load(null);
  }, [load]);
  return { ...state, more: () => state.cursor && void load(state.cursor) };
}

type CountResultLike = { exact: number } | { at_least: number };

/// レーンの件数の表示: レーンごとの件数（サーバの集計）があればそれ、無ければ読み込み済みの数（続きがあれば
/// 「+」を付けて、読み込み済みの数がレーンの総数に見えないようにする・A P3）。
function laneLabel(c: CountResultLike | undefined, loaded: number, more: boolean): string {
  if (c && "exact" in c) return c.exact.toLocaleString("ja-JP");
  if (c && "at_least" in c) return `${c.at_least.toLocaleString("ja-JP")}+`;
  return more ? `${loaded.toLocaleString("ja-JP")}+` : loaded.toLocaleString("ja-JP");
}

/// レーンごとの件数を別リクエストで数える（行の描画を待たせない・閲覧者本人の権限で数える）。
function useLaneCounts(tableId: string, query: DataQuery, field: string, lanes: string[]) {
  const [counts, setCounts] = React.useState<Map<string, CountResultLike>>(new Map());
  const key = JSON.stringify([query.filter ?? null, field, lanes]);
  React.useEffect(() => {
    let active = true;
    setCounts(new Map());
    const base = query.filter ?? null;
    if (lanes.length === 0) return;
    void Promise.all(
      lanes.map(async (l) => {
        const lane: Condition = l ? { cmp: { field, op: "eq", value: l } } : { cmp: { field, op: "is_null" } };
        try {
          const res = await runQuery(tableId, {
            filter: base ? { all: [base, lane] } : lane,
            limit: 1,
            with_count: true,
            select: [],
          });
          return [l, res.count ?? null] as const;
        } catch {
          return [l, null] as const;
        }
      }),
    ).then((pairs) => {
      if (!active) return;
      const m = new Map<string, CountResultLike>();
      for (const [l, c] of pairs) if (c) m.set(l, c as CountResultLike);
      setCounts(m);
    });
    return () => {
      active = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tableId, key]);
  return counts;
}

/// カードに出すユーザーの列・作成者の表示名を引き、引けたら描き直す（グリッドと同じ・未解決の id だけ）。
function useUserNames(items: DataRecord[], columns: ColumnModel[]): number {
  const [version, setVersion] = React.useState(0);
  React.useEffect(() => {
    const ids = userRefIds(items, columns).filter((id) => cachedUserName(id) === undefined);
    const roles = roleRefIds(items, columns).filter((id) => cachedRoleName(id) === undefined);
    if (ids.length === 0 && roles.length === 0) return;
    let active = true;
    void Promise.all([resolveUserNames(ids), resolveRoleNames(roles)]).then(([u, r]) => {
      if (active && (u || r)) setVersion((v) => v + 1);
    });
    return () => {
      active = false;
    };
  }, [items, columns]);
  return version;
}

function MoreButton({ cursor, loading, onMore }: { cursor: string | null; loading: boolean; onMore: () => void }) {
  if (!cursor && !loading) return null;
  return (
    <div className="py-3 text-center">
      <Button variant="ghost" size="sm" onClick={onMore} disabled={loading}>
        {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
        {loading ? "読み込み中…" : "さらに読み込む"}
      </Button>
    </div>
  );
}

function Card({ r, title, fields }: { r: DataRecord; title: ColumnModel | undefined; fields: ColumnModel[] }) {
  const t = title ? formatValue(cellValue(r, title.key), title.field, title.key) : "";
  return (
    <article className="rounded-lg border bg-card p-3 shadow-xs transition-shadow hover:shadow-sm">
      <h4 className="truncate text-sm font-medium">{t || <span className="text-muted-foreground">（無題）</span>}</h4>
      {fields.length ? (
        <dl className="mt-2 space-y-1">
          {fields.map((c) => {
            const { text: v, muted } = displayCell(r, c, cachedUserName, cachedRoleName);
            return (
              <div key={c.key} className="flex gap-2 text-xs">
                <dt className="w-20 shrink-0 truncate text-muted-foreground">{c.label}</dt>
                <dd className={cn("min-w-0 flex-1 truncate", muted && "italic text-muted-foreground")}>
                  {v || <span className="text-muted-foreground/60">—</span>}
                </dd>
              </div>
            );
          })}
        </dl>
      ) : null}
    </article>
  );
}

export function KanbanBoard({
  tableId,
  spec,
  columns,
  query,
  onError,
}: {
  tableId: string;
  spec: Extract<ViewSpec, { kind: "kanban" }>;
  columns: ColumnModel[];
  query: DataQuery;
  onError: (e: Error) => void;
}) {
  const { items, cursor, loading, more } = useRecords(tableId, query, onError);
  useUserNames(items, columns);
  const byKey = new Map(columns.map((c) => [c.key, c]));
  const group = byKey.get(spec.group_field);
  const cards = (spec.card_fields ?? []).map((k) => byKey.get(k)).filter((c): c is ColumnModel => Boolean(c));
  const [title, ...rest] = cards;
  const lanes = [...(group?.field?.options ?? []), ""];
  // 件数は索引のある列だけ数える（未索引の列の絞り込みは 400 になる。数えられなければ読み込み済みの数＋「+」）。
  const laneCounts = useLaneCounts(tableId, query, spec.group_field, group?.field?.indexed ? lanes : []);
  const grouped = new Map<string, DataRecord[]>(lanes.map((l) => [l, []]));
  for (const r of items) {
    const v = cellValue(r, spec.group_field);
    const k = typeof v === "string" && grouped.has(v) ? v : "";
    grouped.get(k)!.push(r);
  }
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex min-h-0 flex-1 gap-3 overflow-x-auto pb-2 scrollbar-subtle" data-testid="kanban-board">
        {lanes
          .filter((l) => !(spec.hide_empty && (grouped.get(l)?.length ?? 0) === 0))
          .map((l) => (
            <section key={l || "__none"} className="flex w-72 shrink-0 flex-col rounded-xl bg-muted/50 p-2">
              <header className="flex items-center justify-between px-1.5 pb-2 pt-1">
                <h3 className="truncate text-xs font-semibold">{l || "（未設定）"}</h3>
                <span className="text-[11px] tabular-nums text-muted-foreground" data-testid="kanban-lane-count">
                  {laneLabel(laneCounts.get(l), grouped.get(l)?.length ?? 0, cursor !== null)}
                </span>
              </header>
              <div className="min-h-0 flex-1 space-y-2 overflow-y-auto scrollbar-subtle">
                {(grouped.get(l) ?? []).map((r) => (
                  <Card key={r.id} r={r} title={title} fields={rest} />
                ))}
              </div>
            </section>
          ))}
      </div>
      <MoreButton cursor={cursor} loading={loading} onMore={more} />
    </div>
  );
}

export function GalleryGrid({
  tableId,
  spec,
  columns,
  query,
  onError,
}: {
  tableId: string;
  spec: Extract<ViewSpec, { kind: "gallery" }>;
  columns: ColumnModel[];
  query: DataQuery;
  onError: (e: Error) => void;
}) {
  const { items, cursor, loading, more } = useRecords(tableId, query, onError);
  useUserNames(items, columns);
  const byKey = new Map(columns.map((c) => [c.key, c]));
  const title = spec.title_field ? byKey.get(spec.title_field) : columns.find((c) => !c.system);
  const fields = (spec.card_fields ?? []).map((k) => byKey.get(k)).filter((c): c is ColumnModel => Boolean(c));
  return (
    <div className="h-full overflow-y-auto pb-4 scrollbar-subtle" data-testid="gallery-grid">
      <div className="grid grid-cols-[repeat(auto-fill,minmax(14rem,1fr))] gap-3">
        {items.map((r) => (
          <Card key={r.id} r={r} title={title} fields={fields} />
        ))}
      </div>
      {!loading && items.length === 0 ? (
        <p className="py-16 text-center text-sm text-muted-foreground">
          {query.filter ? "条件に合うレコードはありません" : "まだレコードがありません"}
        </p>
      ) : null}
      <MoreButton cursor={cursor} loading={loading} onMore={more} />
    </div>
  );
}

const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"];

function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/// カレンダーに並べられる列（日付・日時と、作成日時・更新日時）。
export function calendarDateColumns(columns: ColumnModel[]): ColumnModel[] {
  return columns.filter(
    (c) =>
      c.field?.type === "date" ||
      c.field?.type === "date_time" ||
      (!c.field && (c.key === "_created_at" || c.key === "_updated_at")),
  );
}

/// 1 日の予定（3 件まで並べ、残りは「ほか n 件」から全部を開く）。
function DayEvents({
  day,
  events,
  label,
  max,
}: {
  day: Date;
  events: DataRecord[];
  label: (r: DataRecord) => string;
  max: number;
}) {
  // 入りきらない日は max - 1 件と「ほか n 件」（ちょうど max 件なら全部並べる）。
  const shown = events.length > max ? max - 1 : max;
  return (
    <ul className="space-y-0.5">
      {events.slice(0, shown).map((r) => (
        <li key={r.id} className="truncate rounded bg-primary/10 px-1 text-[11px] text-primary">
          {label(r)}
        </li>
      ))}
      {events.length > shown ? (
        <li>
          <Popover>
            <PopoverTrigger asChild>
              <button
                type="button"
                className="w-full truncate rounded px-1 text-left text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                data-testid="calendar-more"
              >
                ほか {events.length - shown} 件
              </button>
            </PopoverTrigger>
            <PopoverContent align="start" className="w-64 p-2">
              <p className="px-1 pb-1.5 text-xs font-semibold">
                {day.getMonth() + 1} 月 {day.getDate()} 日（{events.length} 件）
              </p>
              <ul className="max-h-64 space-y-0.5 overflow-y-auto scrollbar-subtle" data-testid="calendar-day-list">
                {events.map((r) => (
                  <li key={r.id} className="truncate rounded bg-primary/10 px-1.5 py-0.5 text-xs text-primary">
                    {label(r)}
                  </li>
                ))}
              </ul>
            </PopoverContent>
          </Popover>
        </li>
      ) : null}
    </ul>
  );
}

export function CalendarMonth({
  tableId,
  spec,
  columns,
  query,
  onError,
  onSpecChange,
}: {
  tableId: string;
  spec: Extract<ViewSpec, { kind: "calendar" }>;
  columns: ColumnModel[];
  query: DataQuery;
  onError: (e: Error) => void;
  /// 並べる日付の列を変える（ビューの表示設定・保存はビューの保存で）。
  onSpecChange?: (spec: Extract<ViewSpec, { kind: "calendar" }>) => void;
}) {
  const [month, setMonth] = React.useState(() => {
    const d = new Date();
    return new Date(d.getFullYear(), d.getMonth(), 1);
  });
  const next = new Date(month.getFullYear(), month.getMonth() + 1, 1);
  const byKey = new Map(columns.map((c) => [c.key, c]));
  const dateCol = byKey.get(spec.date_field);
  const isDateOnly = dateCol?.field?.type === "date";
  const lo = isDateOnly ? ymd(month) : month.toISOString();
  const hi = isDateOnly ? ymd(next) : next.toISOString();
  const range: Condition = {
    all: [
      { cmp: { field: spec.date_field, op: "gte", value: lo } },
      { cmp: { field: spec.date_field, op: "lt", value: hi } },
    ],
  };
  const q: DataQuery = {
    ...query,
    filter: query.filter ? { all: [query.filter, range] } : range,
    sort: [{ field: spec.date_field, descending: false }],
  };
  const { items, cursor, loading, more } = useRecords(tableId, q, onError);
  const title = spec.title_field ? byKey.get(spec.title_field) : columns.find((c) => !c.system);
  const byDay = new Map<string, DataRecord[]>();
  for (const r of items) {
    const v = cellValue(r, spec.date_field);
    if (typeof v !== "string") continue;
    const day = isDateOnly ? v.slice(0, 10) : ymd(new Date(v));
    byDay.set(day, [...(byDay.get(day) ?? []), r]);
  }
  const start = new Date(month);
  start.setDate(1 - month.getDay());
  const cells = Array.from({ length: 42 }, (_, i) => {
    const d = new Date(start);
    d.setDate(start.getDate() + i);
    return d;
  });
  const today = ymd(new Date());
  const narrow = useMediaQuery("(max-width: 639.98px)");
  const dateColumns = calendarDateColumns(columns);
  const eventLabel = (r: DataRecord) =>
    title ? formatValue(cellValue(r, title.key), title.field, title.key) || "（無題）" : "（無題）";
  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="calendar-month">
      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1.5">
        <Button variant="ghost" size="icon" className="size-8" aria-label="前の月" onClick={() => setMonth(new Date(month.getFullYear(), month.getMonth() - 1, 1))}>
          <ChevronLeft className="size-4" aria-hidden />
        </Button>
        <h3 className="min-w-28 text-center text-sm font-semibold tabular-nums">
          {month.getFullYear()} 年 {month.getMonth() + 1} 月
        </h3>
        <Button variant="ghost" size="icon" className="size-8" aria-label="次の月" onClick={() => setMonth(next)}>
          <ChevronRight className="size-4" aria-hidden />
        </Button>
        {loading ? <Loader2 className="size-4 animate-spin text-muted-foreground" aria-label="読み込み中" /> : null}
        <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground" data-testid="calendar-count" aria-live="polite">
          {loading && items.length === 0 ? "" : `この月 ${items.length.toLocaleString("ja-JP")}${cursor ? "+" : ""} 件`}
        </span>
        {onSpecChange && dateColumns.length > 0 ? (
          <div className="ml-auto flex items-center gap-1.5">
            <span className="text-xs text-muted-foreground max-sm:sr-only">日付の列</span>
            <Select value={spec.date_field} onValueChange={(date_field) => onSpecChange({ ...spec, date_field })}>
              <SelectTrigger aria-label="カレンダーに並べる日付の列" className="h-8 w-auto min-w-28 gap-1.5 text-[13px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {dateColumns.map((c) => (
                  <SelectItem key={c.key} value={c.key}>
                    {c.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        ) : null}
      </div>
      <div className="grid grid-cols-7 border-b text-center text-[11px] text-muted-foreground">
        {WEEKDAYS.map((w) => (
          <div key={w} className="py-1">
            {w}
          </div>
        ))}
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-7 grid-rows-6 overflow-hidden rounded-b-lg border-x border-b">
        {cells.map((d) => {
          const key = ymd(d);
          const inMonth = d.getMonth() === month.getMonth();
          const evs = byDay.get(key) ?? [];
          return (
            <div key={key} className={cn("min-h-0 overflow-hidden border-r border-t p-1 [&:nth-child(7n)]:border-r-0", !inMonth && "bg-muted/40")}>
              <div className={cn("mb-0.5 text-[11px] tabular-nums", key === today ? "font-semibold text-primary" : "text-muted-foreground")}>
                {d.getDate()}
              </div>
              <DayEvents day={d} events={evs} label={eventLabel} max={narrow ? 2 : 4} />
            </div>
          );
        })}
      </div>
      <MoreButton cursor={cursor} loading={false} onMore={more} />
    </div>
  );
}
