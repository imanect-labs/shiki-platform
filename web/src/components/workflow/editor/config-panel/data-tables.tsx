"use client";

/// ワークフローの設定で使うテーブルの一覧と、テーブルを選ぶ欄（data.* のブロック・承認を待つブロック・データ変更の
/// きっかけ・有効化の画面が共通に使う）。
///
/// 一覧（GET /data/tables）は更新の新しい順に 200 件で打ち切る。束縛済みのテーブルがその中に無いと、一覧から探す
/// 欄は空欄に見え、列・状態の選択肢も出ない（BUG-68）。そこで一覧に無い束縛済みの id は 1 件ずつ
/// GET /data/tables/{id}（一覧と同じく、閲覧者の見え方で絞った定義が返る）で取り、一覧に足して扱う。読めない id は
/// 空欄のまま黙らずにそう出す（サーバは見えないテーブルにも、無い・削除済みと同じ 404 を返すので、どちらかは
/// 言い分けない）。
///
/// 選ぶ欄は名前で探せる（GAP-42）。先頭の 200 件より先のテーブルは、探す欄に名前を入れるとサーバで探し
/// （`?q=`）、続きは「さらに読み込む」で足す（`table-search-list.tsx`）。

import { ChevronDown } from "lucide-react";
import * as React from "react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { useMediaQuery } from "@/hooks/use-media-query";
import { DataApiError, getTable, listTablesPage, type DataTable } from "@/lib/data-api";
import { cn } from "@/lib/utils";
import { TableSearchList } from "./table-search-list";

/// id のテーブルの読み込みの状態。
export type TableLookup =
  /// 束縛していない。
  | { state: "none" }
  | { state: "loading" }
  | { state: "ok"; table: DataTable }
  /// 見えない・無い・削除された（404）、または id の形が違う（400）。
  | { state: "unreadable" }
  /// それ以外の誤り（通信・サーバ）。
  | { state: "failed" };

export type DataTables = {
  /// 選べるテーブル（一覧に、一覧に無かった束縛済みのテーブルと、探して選んだテーブルを足したもの）。一覧の
  /// 読み込み中は null。
  tables: DataTable[] | null;
  /// 一覧の続きの位置（先頭の 200 件で打ち切ったとき・無ければ null）。
  nextCursor: string | null;
  /// 一覧を読めなかったときの誤り（束縛済みのテーブルは、それでも 1 件ずつ取りに行く）。
  error: string | null;
  /// id のテーブル（[`useDataTables`] の `bound` に渡した id と、[`DataTables.remember`] で覚えたものを引く）。
  lookup: (id: string | null | undefined) => TableLookup;
  /// 一覧の外から選んだテーブル（名前で探した結果・続きのページ）を覚える（取り直さずに引けるように）。
  remember: (table: DataTable) => void;
};

type Fetched = DataTable | "loading" | "unreadable" | "failed";

/// 1 件で取りに行く id の形（手で打っている途中の文字では取りに行かない・「.」などで別の URL にならない）。
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// 1 件取りの応答が、頼んだ id のテーブルか（転送などで別の応答が返っても、テーブルとして扱わない）。
function isTableOf(id: string, v: unknown): v is DataTable {
  if (typeof v !== "object" || v === null) return false;
  const t = v as { id?: unknown; schema?: { fields?: unknown } };
  return typeof t.id === "string" && t.id.toLowerCase() === id.toLowerCase() && Array.isArray(t.schema?.fields);
}

/// 自分が使えるテーブルの一覧と、束縛済みのテーブル（パネルを開くたびに一覧を 1 回だけ読む・`enabled` が偽の間は
/// 読まない）。`bound` のうち一覧に無い id だけを 1 件ずつ取る。
export function useDataTables({
  bound = [],
  enabled = true,
}: { bound?: readonly (string | null | undefined)[]; enabled?: boolean } = {}): DataTables {
  const [listed, setListed] = React.useState<DataTable[] | null>(null);
  const [nextCursor, setNextCursor] = React.useState<string | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [fetched, setFetched] = React.useState<ReadonlyMap<string, Fetched>>(() => new Map());
  // 取りに行った id（同じ id を 2 度取らない・応答が来る前の再描画でも）。
  const requested = React.useRef(new Set<string>());
  const alive = React.useRef(true);
  React.useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  React.useEffect(() => {
    if (!enabled) return;
    let active = true;
    listTablesPage()
      .then((p) => {
        if (!active) return;
        setListed(p.items);
        setNextCursor(p.next_cursor ?? null);
      })
      .catch((e) => {
        if (!active) return;
        setListed([]);
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [enabled]);

  const ids = [...new Set(bound.filter((id): id is string => typeof id === "string" && id !== ""))];
  const key = ids.join("\n");
  React.useEffect(() => {
    if (!enabled || listed === null) return;
    const want = key
      .split("\n")
      .filter((id) => id !== "" && !requested.current.has(id) && !listed.some((t) => t.id === id));
    if (!want.length) return;
    for (const id of want) requested.current.add(id);
    const put = (id: string, v: Fetched) => setFetched((m) => new Map(m).set(id, v));
    for (const id of want) {
      // UUID の形でない id（手で打っている途中など）は取りに行かずに読めないとする。
      if (!UUID_RE.test(id)) {
        put(id, "unreadable");
        continue;
      }
      put(id, "loading");
      getTable(id)
        .then((t) => alive.current && put(id, isTableOf(id, t) ? t : "failed"))
        .catch((e) => {
          if (!alive.current) return;
          const status = e instanceof DataApiError ? e.status : 0;
          put(id, status === 400 || status === 403 || status === 404 ? "unreadable" : "failed");
        });
    }
  }, [enabled, listed, key]);

  const remember = React.useCallback((table: DataTable) => {
    // 覚えた id はもう取りに行かない（一覧の外から選んだ直後に、同じテーブルを 1 件取りし直さない）。
    requested.current.add(table.id);
    setFetched((m) => (m.get(table.id) === table ? m : new Map(m).set(table.id, table)));
  }, []);

  const tables = React.useMemo(() => {
    if (listed === null) return null;
    const extra = [...fetched.values()].filter(
      (v): v is DataTable => typeof v === "object" && !listed.some((t) => t.id === v.id),
    );
    return extra.length ? [...listed, ...extra] : listed;
  }, [listed, fetched]);

  const lookup = React.useCallback(
    (id: string | null | undefined): TableLookup => {
      if (!id) return { state: "none" };
      if (tables === null) return { state: "loading" };
      const hit = tables.find((t) => t.id === id);
      if (hit) return { state: "ok", table: hit };
      const f = fetched.get(id);
      // まだ取りに行っていない（束縛が変わった直後の描画）も読み込み中。
      if (f === undefined || f === "loading") return { state: "loading" };
      // 1 件で取れた（返った id の書き方が束縛と違っても、その id で取れたもの）。
      if (typeof f === "object") return { state: "ok", table: f };
      return { state: f };
    },
    [tables, fetched],
  );
  return { tables, nextCursor, error, lookup, remember };
}

/// 束縛済みのテーブルを読めないときの注記。
export const UNREADABLE_TABLE = "このテーブルを読めません（または削除されました）";

/// 設定パネルがシートになる幅（config-panel/index.tsx・model-field.tsx と同じ）。
const PANEL_QUERY = "(min-width: 768px)";

/// テーブルの選択（固定値で選ぶ・名前で探せる）。
///
/// ボタンに今のテーブル名を出し、押すと探す欄つきの一覧を開く。PC はポップオーバー、設定パネルが下からのシートの
/// ときはシートの中に一覧を広げる（モデル欄と同じ）。束縛済みの値が選択肢に無いとき（読み込み中・読めない・
/// `eligible` で外れる）も空欄にしない。ボタンにそう出し、読めない・選べないときは欄の下に理由を出す。
/// `onChange` には選んだテーブルの定義も渡す（名前で探した、先頭の 200 件の外のテーブルでも列・状態を引ける）。
export function DataTableSelect({
  value,
  onChange,
  source,
  eligible,
  ineligibleNote,
  label = "対象のテーブル",
  id,
}: {
  value: string | null;
  onChange: (id: string, table: DataTable) => void;
  source: DataTables;
  /// 選べるテーブル（省略で全部）。
  eligible?: (t: DataTable) => boolean;
  /// 束縛済みのテーブルが `eligible` で外れるときの注記。
  ineligibleNote?: string;
  label?: string;
  id?: string;
}) {
  const desktop = useMediaQuery(PANEL_QUERY);
  const [open, setOpen] = React.useState(false);
  const triggerRef = React.useRef<HTMLButtonElement | null>(null);
  // シートの中の一覧はフォーカスを持ったまま消えるので、閉じたら選ぶ欄へ戻す（ポップオーバーは Radix が戻す）。
  const close = () => {
    setOpen(false);
    if (!desktop) triggerRef.current?.focus({ preventScroll: true });
  };
  const listId = React.useId();
  const loading = source.tables === null;
  const bound = source.lookup(value);
  const ineligible = bound.state === "ok" && eligible !== undefined && !eligible(bound.table);
  const note =
    bound.state === "unreadable"
      ? UNREADABLE_TABLE
      : bound.state === "failed"
        ? "このテーブルを読み込めませんでした"
        : ineligible
          ? (ineligibleNote ?? "このテーブルは選べません")
          : null;
  const shown =
    bound.state === "ok" ? (
      bound.table.name
    ) : bound.state === "loading" || (loading && !value) ? (
      <span className="text-muted-foreground">読み込み中…</span>
    ) : bound.state === "unreadable" || bound.state === "failed" ? (
      <span className="text-destructive">読めないテーブル</span>
    ) : (
      <span className="text-muted-foreground">テーブルを選ぶ</span>
    );
  const pick = (t: DataTable) => {
    source.remember(t);
    onChange(t.id, t);
    close();
  };
  const trigger = (
    <button
      ref={triggerRef}
      type="button"
      id={id}
      role="combobox"
      aria-label={label}
      aria-expanded={open}
      aria-haspopup="listbox"
      aria-controls={open ? listId : undefined}
      aria-invalid={note ? true : undefined}
      disabled={loading}
      data-testid="workflow-table-select"
      onClick={desktop ? undefined : () => setOpen((o) => !o)}
      onKeyDown={(e) => {
        // 選択の欄と同じく ↓↑ でも開く（Enter・Space はボタンの押下）。
        if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !open) {
          e.preventDefault();
          setOpen(true);
        }
      }}
      className={cn(
        "flex h-8 w-full items-center justify-between gap-2 rounded-md border border-input bg-background px-3 text-left text-[13px] shadow-xs pointer-coarse:h-11",
        "transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        "disabled:cursor-not-allowed disabled:opacity-50",
        note && "border-destructive",
      )}
    >
      <span className="min-w-0 flex-1 truncate">{shown}</span>
      <ChevronDown
        className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")}
        aria-hidden
      />
    </button>
  );
  const list = (surface: "popover" | "sheet") => (
    <TableSearchList
      listId={listId}
      source={source}
      value={value}
      eligible={eligible}
      onPick={pick}
      surface={surface}
      label={label}
    />
  );
  return (
    <div className="space-y-1">
      {desktop ? (
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>{trigger}</PopoverTrigger>
          <PopoverContent
            align="start"
            collisionPadding={12}
            className="flex max-h-[min(var(--radix-popover-content-available-height),24rem)] w-[max(var(--radix-popover-trigger-width),18rem)] flex-col p-1.5"
            data-testid="workflow-table-picker"
          >
            {list("popover")}
          </PopoverContent>
        </Popover>
      ) : (
        <>
          {trigger}
          {open ? <InlineList onClose={close}>{list("sheet")}</InlineList> : null}
        </>
      )}
      {note ? (
        <p className="text-[11px] leading-relaxed text-destructive" data-testid="workflow-table-note">
          {note}
        </p>
      ) : null}
    </div>
  );
}

/// シートの中に広げる一覧の枠。開いたら一覧へフォーカスを移す（ソフトキーボードを出さない・モデル欄と同じ）。
function InlineList({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  const ref = React.useRef<HTMLDivElement | null>(null);
  React.useEffect(() => {
    ref.current?.querySelector<HTMLElement>('[role="listbox"]')?.focus({ preventScroll: true });
    ref.current?.scrollIntoView({ block: "nearest" });
  }, []);
  return (
    <div
      ref={ref}
      className="flex max-h-80 flex-col rounded-md border bg-background p-1"
      data-testid="workflow-table-picker"
      // 一覧の中の Esc は一覧だけを閉じる（シートごと閉じない・local-escape.ts）。
      data-local-escape="true"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      {children}
    </div>
  );
}
