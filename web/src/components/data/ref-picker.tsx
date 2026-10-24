"use client";

/// 参照の値（ユーザー・部署・リンク）を選ぶ部品（A-6）。レコードの追加とグリッドのセルの編集で使う。
///
/// - ユーザー・部署は同じテナントのディレクトリから検索する（`/directory/users` / `/directory/roles`）
/// - リンクは参照先テーブルを閲覧者本人の権限で引き、タイトル列（参照先の表示順で先頭のテキスト列・サーバの
///   `link_labels` と同じ規則）で探す。見えない行は候補に出ない（サーバが行の権限で絞る）
/// - 単一の値（ユーザー・部署・多対一/一対一のリンク）は選んだら確定、複数（多対多）はチップで足し引きする
/// キーボード: ↑↓ で候補を移動、Enter で選ぶ（複数は入力が空の Enter で確定）、Backspace で最後のチップを外す。

import { Check, Loader2, X } from "lucide-react";
import * as React from "react";

import type { ProvideEditorComponent, TextCell } from "@glideapps/glide-data-grid";
import { getTable, runQuery, type DataTable, type FieldDef } from "@/lib/data-api";
import { searchDirectory, searchRoles } from "@/lib/storage";
import { cachedUserName } from "@/lib/user-names";
import { cn } from "@/lib/utils";

export type RefOption = { id: string; label: string; sub?: string };

/// 値が複数になる参照か（多対多のリンク）。
export function isMultiRef(f: FieldDef): boolean {
  return f.type === "record_ref" && Boolean(f.link) && (f.link!.cardinality === "many_to_many" || f.link!.cardinality === "one_to_many");
}

/// 参照の型か（ユーザー・部署・リンク。逆向きのリンクは読み取り専用）。
export function isRefField(f: FieldDef | null | undefined): boolean {
  return Boolean(f && (f.type === "user_ref" || f.type === "role_ref" || (f.type === "record_ref" && !f.link?.reverse)));
}

/// 参照先テーブルのタイトル列（先頭のテキスト列）。
function titleField(t: DataTable | null): FieldDef | null {
  if (!t) return null;
  const texts = t.schema.fields.filter((f) => f.type === "text").sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
  return texts[0] ?? null;
}

const refTables = new Map<string, Promise<DataTable | null>>();
function loadRefTable(id: string): Promise<DataTable | null> {
  let p = refTables.get(id);
  if (!p) {
    p = getTable(id).catch(() => null);
    refTables.set(id, p);
    // 失敗はすぐ忘れる（次に開いたときに引き直す）。
    void p.then((t) => !t && refTables.delete(id));
  }
  return p;
}

function useRefSearch(field: FieldDef, q: string) {
  const [state, setState] = React.useState<{ loading: boolean; items: RefOption[]; error: string | null }>({
    loading: true,
    items: [],
    error: null,
  });
  React.useEffect(() => {
    let active = true;
    setState((s) => ({ ...s, loading: true, error: null }));
    const t = window.setTimeout(async () => {
      try {
        let items: RefOption[] = [];
        if (field.type === "user_ref") {
          const r = await searchDirectory({ q, limit: 10 });
          items = r.items.map((u) => ({ id: u.id, label: u.display_name, sub: u.email }));
        } else if (field.type === "role_ref") {
          const r = await searchRoles({ q, limit: 10 });
          items = r.items.map((x) => ({ id: x.id, label: x.display_name }));
        } else if (field.type === "record_ref" && field.ref_table) {
          const ref = await loadRefTable(field.ref_table);
          const title = titleField(ref);
          const needle = q.trim();
          // 索引のあるタイトル列なら「で始まる」をサーバで、無ければ先頭の数十件を手元で絞る。
          const res = await runQuery(field.ref_table, {
            limit: title?.indexed || !needle ? 20 : 100,
            ...(title && needle && title.indexed
              ? { filter: { cmp: { field: title.key, op: title.searchable ? "contains" : "starts_with", value: needle } } }
              : {}),
            ...(title ? { select: [title.key] } : { select: [] }),
          });
          items = (res.items ?? [])
            .map((r) => {
              const v = title ? (r.data as Record<string, unknown> | null)?.[title.key] : null;
              return { id: r.id, label: typeof v === "string" && v ? v : `${r.id.slice(0, 8)}…` };
            })
            .filter((o) => !needle || title?.indexed || o.label.toLowerCase().includes(needle.toLowerCase()))
            .slice(0, 20);
        }
        if (active) setState({ loading: false, items, error: null });
      } catch (e) {
        if (active) setState({ loading: false, items: [], error: e instanceof Error ? e.message : String(e) });
      }
    }, 200);
    return () => {
      active = false;
      window.clearTimeout(t);
    };
  }, [field.type, field.ref_table, q]);
  return state;
}

/// 選んだ値の見出し（id → 表示）。ユーザーはディレクトリの表示名、リンクは行の `link_labels`、無ければ id の先頭。
export function refLabel(field: FieldDef, id: string, labels?: Record<string, string>): string {
  if (labels?.[id]) return labels[id];
  if (field.type === "user_ref") return cachedUserName(id) || `${id.slice(0, 8)}…`;
  return `${id.slice(0, 8)}…`;
}

export function RefPicker({
  field,
  value,
  labels,
  onChange,
  onCommit,
  onCancel,
  autoFocus,
  id,
  compact,
  initialQuery,
}: {
  field: FieldDef;
  value: string[];
  labels?: Record<string, string>;
  onChange: (ids: string[], labels: Record<string, string>) => void;
  /// 確定（単一は選んだとき・複数は空の Enter）。グリッドの編集欄で使う。
  onCommit?: (ids: string[], labels: Record<string, string>) => void;
  onCancel?: () => void;
  autoFocus?: boolean;
  id?: string;
  compact?: boolean;
  /// 打鍵で開いたときの最初の文字（探す欄に入れておく）。
  initialQuery?: string;
}) {
  const multi = isMultiRef(field);
  const [q, setQ] = React.useState(initialQuery ?? "");
  const [active, setActive] = React.useState(0);
  const [known, setKnown] = React.useState<Record<string, string>>(labels ?? {});
  // フォームの中（compact でない）では、探す欄にフォーカスがある間だけ候補を出す（並んだ列の邪魔をしない）。
  // 単一の値を選んだら（打ち直すまで）閉じる。下の列に重なったままにしない。
  const [focused, setFocused] = React.useState(false);
  const [closed, setClosed] = React.useState(false);
  const showList = compact || (focused && !closed);
  const { loading, items, error } = useRefSearch(field, q);
  const inputRef = React.useRef<HTMLInputElement>(null);
  React.useLayoutEffect(() => {
    if (autoFocus) inputRef.current?.focus();
  }, [autoFocus]);
  React.useEffect(() => setActive(0), [q, items.length]);
  // 選んである値の見出しが分からない（id の先頭で出している）ものは、候補に出てきたら覚える。
  React.useEffect(() => {
    const learned = items.filter((o) => value.includes(o.id) && !known[o.id]);
    if (learned.length) setKnown((k) => ({ ...k, ...Object.fromEntries(learned.map((o) => [o.id, o.label])) }));
  }, [items, value, known]);

  const pick = (o: RefOption) => {
    const nextLabels = { ...known, [o.id]: o.label };
    setKnown(nextLabels);
    if (multi) {
      const next = value.includes(o.id) ? value.filter((x) => x !== o.id) : [...value, o.id];
      onChange(next, nextLabels);
      setQ("");
    } else {
      onChange([o.id], nextLabels);
      setQ("");
      setClosed(true);
      onCommit?.([o.id], nextLabels);
    }
  };
  const listId = `${id ?? "ref"}-list`;

  return (
    <div className={cn("space-y-1.5", compact && "w-72 p-1.5")}>
      {value.length ? (
        <ul className="flex flex-wrap gap-1" aria-label="選んだ値">
          {value.map((v) => (
            <li key={v} className="inline-flex max-w-full items-center gap-1 rounded-full border bg-card py-0.5 pl-2 pr-0.5 text-xs">
              <span className="truncate">{refLabel(field, v, known)}</span>
              <button
                type="button"
                aria-label={`「${refLabel(field, v, known)}」を外す`}
                onClick={() => onChange(value.filter((x) => x !== v), known)}
                className="rounded-full p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3" aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="relative">
      <input
        ref={inputRef}
        id={id}
        role="combobox"
        aria-expanded={showList}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={items[active] ? `${listId}-${active}` : undefined}
        value={q}
        onFocus={() => {
          setFocused(true);
          setClosed(false);
        }}
        onBlur={() => setFocused(false)}
        onChange={(e) => {
          setQ(e.target.value);
          setClosed(false);
        }}
        placeholder={field.type === "user_ref" ? "名前・メールで探す" : field.type === "role_ref" ? "部署を探す" : "探す"}
        className="h-8 w-full rounded-md border border-input bg-background px-2.5 text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing) return;
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setClosed(false);
            setActive((a) => Math.min(items.length - 1, a + 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(0, a - 1));
          } else if (e.key === "Enter") {
            e.preventDefault();
            e.stopPropagation();
            if (q.trim() && items[active]) pick(items[active]);
            else if (multi || !q.trim()) onCommit?.(value, known);
          } else if (e.key === "Backspace" && !q && value.length) {
            onChange(value.slice(0, -1), known);
          } else if (e.key === "Escape" && onCancel) {
            e.preventDefault();
            e.stopPropagation();
            onCancel();
          } else if (e.key === "Escape" && showList && !compact) {
            // フォームの中の Esc は候補だけを閉じる（ダイアログは閉じない）。
            e.preventDefault();
            e.stopPropagation();
            setClosed(true);
          }
        }}
      />
      {showList ? (
      <ul
        id={listId}
        role="listbox"
        aria-label="候補"
        className={cn(
          "max-h-56 overflow-y-auto rounded-md border bg-popover p-1 scrollbar-subtle",
          // フォームの中では入力欄の下に重ねて出す（下の列を押し下げない）。
          !compact && "absolute inset-x-0 top-full z-50 mt-1 shadow-md",
        )}
      >
        {loading ? (
          <li className="flex items-center gap-1.5 px-2 py-1.5 text-xs text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
            探しています…
          </li>
        ) : error ? (
          <li className="px-2 py-1.5 text-xs text-destructive">{error}</li>
        ) : items.length === 0 ? (
          <li className="px-2 py-1.5 text-xs text-muted-foreground">見つかりません</li>
        ) : (
          items.map((o, i) => {
            const selected = value.includes(o.id);
            return (
              <li
                key={o.id}
                id={`${listId}-${i}`}
                role="option"
                aria-selected={selected}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(o)}
                onMouseEnter={() => setActive(i)}
                className={cn(
                  "flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-[13px]",
                  i === active && "bg-accent",
                )}
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate">{o.label}</span>
                  {o.sub ? <span className="block truncate text-[11px] text-muted-foreground">{o.sub}</span> : null}
                </span>
                {selected ? <Check className="size-3.5 shrink-0 text-primary" aria-hidden /> : null}
              </li>
            );
          })
        )}
      </ul>
      ) : null}
      </div>
      {multi && onCommit ? (
        <div className="flex justify-end gap-1.5 pt-0.5">
          <button
            type="button"
            onClick={() => onCancel?.()}
            className="rounded-md px-2 py-1 text-xs text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            やめる
          </button>
          <button
            type="button"
            onClick={() => onCommit(value, known)}
            className="rounded-md bg-primary px-2.5 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            確定
          </button>
        </div>
      ) : null}
    </div>
  );
}

/// グリッドのセルの参照の値（行の値と見出し）。編集欄はこれを読んで、確定した値を JSON で返す。
export type RefCellPayload = { ids: string[]; labels: Record<string, string> };

/// グリッドの参照のセルの編集欄（glide の provideEditor 用）。確定すると `data` に {@link RefCellPayload} の
/// JSON を入れて返す（`parseInput` が読む）。
export function refCellEditor(
  field: FieldDef,
  current: RefCellPayload,
): { editor: ProvideEditorComponent<TextCell>; disablePadding: true; disableStyling: true } {
  const Editor: ProvideEditorComponent<TextCell> = ({ value, onChange, onFinishedEditing, initialValue }) => {
    const [ids, setIds] = React.useState(current.ids);
    const commit = (next: string[], labels: Record<string, string>) => {
      const same = next.length === current.ids.length && next.every((x, i) => x === current.ids[i]);
      onFinishedEditing(same ? undefined : { ...value, data: JSON.stringify({ ids: next, labels } satisfies RefCellPayload) }, [0, 0]);
    };
    return (
      <div className="rounded-lg border bg-popover text-popover-foreground shadow-md" data-testid="ref-cell-editor">
        <RefPicker
          field={field}
          value={ids}
          labels={current.labels}
          onChange={(next, labels) => {
            setIds(next);
            // 編集欄の外をクリックしたときも確定する（glide は外のクリックで今の値を確定する・Esc は取消）。
            // 元に戻した値も渡す（同じ値なら onCellEdited が保存しない）。
            onChange({ ...value, data: JSON.stringify({ ids: next, labels } satisfies RefCellPayload) });
          }}
          onCommit={commit}
          onCancel={() => onFinishedEditing(undefined, [0, 0])}
          autoFocus
          compact
          initialQuery={initialValue}
        />
      </div>
    );
  };
  Editor.displayName = "RefCellEditor";
  // セルの枠に合わせた既定の編集欄の装飾は使わない（候補の一覧を出すので広さが要る）。
  return { editor: Editor, disablePadding: true, disableStyling: true };
}
