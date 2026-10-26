"use client";

/// テーブルを名前で探して選ぶ一覧（[`DataTableSelect`] の中身・GAP-42）。
///
/// - 探す欄が空: 一覧（GET /data/tables の先頭の 200 件・更新の新しい順）に、束縛済みで一覧に無かったテーブルを
///   足して出す。一覧を打ち切っていれば「さらに読み込む」で続きを足し、名前で探すよう促す
/// - 文字を入れる: サーバで名前を探す（`?q=`・自分が使えるテーブルの中だけ・大文字小文字を区別しない・50 件ずつ）。
///   続きがあれば「さらに読み込む」で足す
/// - キーボード: ↑↓ で候補を移動、Home / End（一覧にフォーカスがあるとき）、Enter で選ぶ。日本語入力の変換中の
///   Enter・矢印は奪わない。Esc は外側（ポップオーバー・シートの中の一覧）が閉じる
///
/// [`DataTableSelect`]: ./data-tables.tsx

import { Check, Loader2, Search } from "lucide-react";
import * as React from "react";

import { listTablesPage, type DataTable } from "@/lib/data-api";
import { cn } from "@/lib/utils";
import type { DataTables } from "./data-tables";

/// 名前で探すときの 1 ページの件数。
const SEARCH_PAGE = 50;
/// 打鍵が止まってから探しに行くまで。
const DEBOUNCE_MS = 200;

type Found = {
  /// 探した語（結果がどの語のものか）。
  q: string;
  items: DataTable[];
  next: string | null;
  loading: boolean;
  error: string | null;
};

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function dedupe(tables: DataTable[]): DataTable[] {
  const seen = new Set<string>();
  return tables.filter((t) => (seen.has(t.id) ? false : (seen.add(t.id), true)));
}

export function TableSearchList({
  listId,
  source,
  value,
  eligible,
  onPick,
  surface,
  label,
}: {
  /// 一覧（listbox）の id（選ぶ欄の aria-controls が指す）。
  listId: string;
  source: DataTables;
  value: string | null;
  eligible?: (t: DataTable) => boolean;
  onPick: (t: DataTable) => void;
  /// 見出しを貼り付けるときの背景（親の面と揃える）。
  surface: "popover" | "sheet";
  /// 欄の名前（一覧の読み上げ名に使う）。
  label: string;
}) {
  const [rawQ, setQ] = React.useState("");
  const q = rawQ.trim();
  const [found, setFound] = React.useState<Found | null>(null);
  // 探す欄が空のときに足した続きのページ（next が undefined なら、続きの位置は一覧のもの）。
  const [more, setMore] = React.useState<{ items: DataTable[]; next?: string | null; loading: boolean; error: string | null }>({
    items: [],
    loading: false,
    error: null,
  });
  const [retry, setRetry] = React.useState(0);

  // 名前で探す（打鍵が止まってから・古い語の応答は捨てる）。
  React.useEffect(() => {
    if (!q) {
      setFound(null);
      return;
    }
    setFound({ q, items: [], next: null, loading: true, error: null });
    const ctrl = new AbortController();
    const timer = window.setTimeout(() => {
      listTablesPage({ q, limit: SEARCH_PAGE }, { signal: ctrl.signal })
        .then((p) => setFound({ q, items: p.items, next: p.next_cursor ?? null, loading: false, error: null }))
        .catch((e) => {
          if (ctrl.signal.aborted) return;
          setFound({ q, items: [], next: null, loading: false, error: errorText(e) });
        });
    }, DEBOUNCE_MS);
    return () => {
      window.clearTimeout(timer);
      ctrl.abort();
    };
  }, [q, retry]);

  const searching = q !== "";
  const base = searching ? (found?.q === q ? found.items : []) : dedupe([...(source.tables ?? []), ...more.items]);
  const items = eligible ? base.filter(eligible) : base;
  const next = searching ? (found?.q === q ? found.next : null) : more.next === undefined ? source.nextCursor : more.next;
  const loading = searching ? !found || found.q !== q || found.loading : source.tables === null;
  const error = searching ? (found?.q === q ? found.error : null) : more.error;
  const loadingMore = searching ? false : more.loading;
  const [paging, setPaging] = React.useState(false);

  const loadMore = async () => {
    if (!next || paging) return;
    setPaging(true);
    try {
      if (searching) {
        const p = await listTablesPage({ q, cursor: next, limit: SEARCH_PAGE });
        setFound((f) =>
          f && f.q === q ? { ...f, items: dedupe([...f.items, ...p.items]), next: p.next_cursor ?? null } : f,
        );
      } else {
        setMore((m) => ({ ...m, loading: true, error: null }));
        const p = await listTablesPage({ cursor: next, limit: SEARCH_PAGE });
        setMore((m) => ({ items: [...m.items, ...p.items], next: p.next_cursor ?? null, loading: false, error: null }));
      }
    } catch (e) {
      if (searching) setFound((f) => (f && f.q === q ? { ...f, error: errorText(e) } : f));
      else setMore((m) => ({ ...m, loading: false, error: errorText(e) }));
    } finally {
      setPaging(false);
    }
  };

  const [active, setActive] = React.useState(() => Math.max(0, items.findIndex((t) => t.id === value)));
  const at = items.length ? Math.min(active, items.length - 1) : -1;
  const optionId = (i: number) => `${listId}-o${i}`;
  const activeId = at >= 0 ? optionId(at) : undefined;
  // キーボードで動かしたときと開いた直後だけ、注目中の行を見える位置へ（マウスで動かしたときに一覧がずれない）。
  const reveal = React.useRef(true);
  React.useEffect(() => {
    if (!activeId || !reveal.current) return;
    document.getElementById(activeId)?.scrollIntoView({ block: "nearest" });
    reveal.current = false;
  }, [activeId]);
  const moveTo = (i: number) => {
    reveal.current = true;
    setActive(i);
  };

  const onKeyDown = (e: React.KeyboardEvent, fromList: boolean) => {
    // 日本語入力の変換中の Enter・矢印は IME のもの（確定・候補選択）。奪わない。
    if (e.nativeEvent.isComposing || items.length === 0) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      moveTo((at + step + items.length) % items.length);
    } else if (fromList && e.key === "Home") {
      e.preventDefault();
      moveTo(0);
    } else if (fromList && e.key === "End") {
      e.preventDefault();
      moveTo(items.length - 1);
    } else if (e.key === "Enter" && at >= 0) {
      e.preventDefault();
      onPick(items[at]);
    }
  };

  const status = loading
    ? searching
      ? "探しています…"
      : "読み込み中…"
    : error
      ? null
      : searching
        ? `${items.length}${next ? " 件以上" : " 件"}`
        : "";

  return (
    <>
      <div className="relative shrink-0 px-1 pb-1.5">
        <Search
          className="pointer-events-none absolute left-3.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          aria-hidden
        />
        <input
          type="text"
          role="combobox"
          aria-expanded
          aria-controls={listId}
          aria-activedescendant={activeId}
          aria-autocomplete="list"
          aria-label="テーブルを名前で探す"
          placeholder="名前で探す"
          value={rawQ}
          onChange={(e) => {
            setQ(e.target.value);
            moveTo(0);
          }}
          onKeyDown={(e) => onKeyDown(e, false)}
          data-testid="workflow-table-search"
          className={cn(
            // iOS は 16px 未満の入力欄にフォーカスすると画面を拡大するので、狭い画面は 16px。
            "h-10 w-full rounded-md border bg-transparent pl-8 pr-2.5 text-base outline-none sm:h-9 sm:text-[13px]",
            "placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring",
          )}
        />
      </div>
      {/* 探した結果の件数・読み込み中を読み上げる。 */}
      <span className="sr-only" aria-live="polite">
        {status}
      </span>
      <div
        id={listId}
        role="listbox"
        aria-label={label}
        aria-busy={loading || undefined}
        aria-activedescendant={activeId}
        // 狭い画面では開いた直後のフォーカスはここ（ソフトキーボードを出さない）なので、一覧もキー操作を受ける。
        tabIndex={-1}
        onKeyDown={(e) => onKeyDown(e, true)}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain outline-none"
      >
        {items.map((t, i) => {
          const selected = t.id === value;
          return (
            <div
              key={t.id}
              id={optionId(i)}
              role="option"
              aria-selected={selected}
              data-active={i === at}
              onMouseMove={() => {
                if (i !== at) setActive(i);
              }}
              onClick={() => onPick(t)}
              className={cn(
                // 指で押す狭い画面は 44px、ポインタの広い画面は詰める。
                "flex h-11 cursor-pointer select-none items-center gap-2 rounded-md px-2.5 text-sm sm:h-9 sm:text-[13px]",
                "data-[active=true]:bg-accent",
                selected && "font-medium",
              )}
            >
              <span className="min-w-0 flex-1 truncate">{t.name}</span>
              {selected ? <Check className="size-4 shrink-0 text-primary" aria-hidden /> : null}
            </div>
          );
        })}
        {loading && items.length === 0 ? (
          <p className="flex items-center justify-center gap-2 px-2.5 py-6 text-[13px] text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            {searching ? "探しています…" : "読み込み中…"}
          </p>
        ) : null}
        {!loading && !error && items.length === 0 ? (
          <p className="px-2.5 py-6 text-center text-[13px] leading-relaxed text-muted-foreground">
            {searching
              ? next
                ? `「${q}」に当たるテーブルのうち、ここで選べるものはまだありません。さらに読み込むか、名前を詳しく入れてください`
                : `「${q}」に当たるテーブルはありません`
              : next
                ? "ここで選べるテーブルはまだありません。名前で探すか、さらに読み込んでください"
                : "使えるテーブルがありません"}
          </p>
        ) : null}
      </div>
      {error ? (
        <div className="shrink-0 space-y-1 px-2.5 pb-1 pt-1.5 text-[12px] leading-relaxed text-destructive" role="alert">
          <p>{searching ? `探せませんでした: ${error}` : `続きを読み込めませんでした: ${error}`}</p>
          {searching ? (
            <button
              type="button"
              onClick={() => setRetry((n) => n + 1)}
              className="rounded px-1 font-medium underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              もう一度探す
            </button>
          ) : null}
        </div>
      ) : null}
      {next && !loading ? (
        <div
          className={cn(
            "shrink-0 space-y-1 border-t px-1 pt-1.5",
            surface === "popover" ? "bg-popover" : "bg-background",
          )}
        >
          {!searching ? (
            <p className="px-1.5 text-[11.5px] leading-relaxed text-muted-foreground">
              更新の新しい順に {base.length} 件を出しています。ほかのテーブルは名前で探してください
            </p>
          ) : null}
          <button
            type="button"
            onClick={() => void loadMore()}
            disabled={paging || loadingMore}
            data-testid="workflow-table-more"
            className={cn(
              "flex h-9 w-full items-center justify-center gap-2 rounded-md text-[13px] text-primary",
              "hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60",
            )}
          >
            {paging || loadingMore ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : null}
            さらに読み込む
          </button>
        </div>
      ) : null}
    </>
  );
}
