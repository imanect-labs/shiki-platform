"use client";

/// SQL 画面の参照一覧（#978）: 閲覧者に見えるテーブルと列。押すと入力欄のカーソル位置へ名前を入れる。
///
/// 一覧は `GET /data/tables`（閲覧者に読める列だけに絞った定義・スキーマ管理者には全列）。見えないテーブルは
/// 出ない（存在を明かさない）。

import { ChevronRight, Link2, RotateCw, Search, Table2 } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { DataTable, FieldDef } from "@/lib/data-api";
import { FIELD_TYPE_LABELS } from "@/lib/data-view";
import { cn } from "@/lib/utils";

/// SQL の予約語（そのまま書くと構文として読まれる名前）。
const RESERVED = new Set(
  "select from where group by order having limit offset join inner left right outer full cross on and or not in is null like between as asc desc distinct union all with case when then else end exists true false table".split(
    " ",
  ),
);

/// SQL に書ける名前（識別子として書けない文字を含む・予約語なら二重引用符で囲む）。
export function sqlIdent(name: string): string {
  return /^[\p{L}_][\p{L}\p{N}_]*$/u.test(name) && !RESERVED.has(name.toLowerCase())
    ? name
    : `"${name.replaceAll('"', '""')}"`;
}

function FieldRow({
  field,
  tableNames,
  onInsert,
}: {
  field: FieldDef;
  tableNames: Map<string, string>;
  onInsert: (text: string) => void;
}) {
  const target = field.ref_table ? tableNames.get(field.ref_table) : undefined;
  const label = field.display_name && field.display_name !== field.key ? field.display_name : null;
  return (
    <li>
      <button
        type="button"
        onClick={() => onInsert(sqlIdent(field.key))}
        className="flex w-full min-w-0 items-baseline gap-2 rounded-md px-2 py-1 text-left transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        title={`「${field.key}」を入れる${label ? `（${label}）` : ""}`}
      >
        <code className="shrink-0 font-mono text-[12.5px] text-foreground">{field.key}</code>
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {label ? <span className="text-foreground/80">{label}</span> : null}
          <span className="ml-1 rounded bg-muted px-1 py-px text-[10.5px]">{FIELD_TYPE_LABELS[field.type] ?? field.type}</span>
          {target ? (
            <span className="ml-1">
              <Link2 className="inline size-3 align-[-1px]" aria-hidden /> {target}
            </span>
          ) : null}
        </span>
      </button>
    </li>
  );
}

export function SqlReference({
  tables,
  failed = false,
  onRetry,
  onInsert,
  focusId,
  className,
}: {
  tables: DataTable[] | null;
  failed?: boolean;
  onRetry?: () => void;
  onInsert: (text: string) => void;
  /// 開いた状態で出すテーブル（テーブルの画面から開いたとき）。
  focusId?: string;
  className?: string;
}) {
  const [query, setQuery] = React.useState("");
  const [open, setOpen] = React.useState<Set<string>>(() => new Set(focusId ? [focusId] : []));
  const tableNames = React.useMemo(() => new Map((tables ?? []).map((t) => [t.id, t.name])), [tables]);
  const q = query.trim().toLowerCase();
  const visible = (tables ?? []).filter(
    (t) =>
      !q ||
      t.name.toLowerCase().includes(q) ||
      t.schema.fields.some((f) => f.key.toLowerCase().includes(q) || (f.display_name ?? "").toLowerCase().includes(q)),
  );
  const toggle = (id: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <section className={cn("flex min-h-0 flex-col gap-2", className)} aria-label="テーブルと列" data-testid="sql-reference">
      <p className="px-1 text-[11px] text-muted-foreground">押すとカーソル位置に入ります</p>
      <div className="relative">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="テーブル・列を探す"
          aria-label="テーブル・列を探す"
          className="h-8 pl-8 text-[13px]"
        />
      </div>
      {tables === null ? (
        <p className="px-1 py-3 text-xs text-muted-foreground" role="status">
          読み込み中…
        </p>
      ) : failed ? (
        <div className="space-y-2 px-1 py-2" role="alert">
          <p className="text-xs text-muted-foreground">テーブルの一覧を読み込めませんでした。</p>
          {onRetry ? (
            <Button variant="outline" size="sm" className="h-7 text-xs" onClick={onRetry}>
              <RotateCw className="size-3.5" aria-hidden />
              読み込み直す
            </Button>
          ) : null}
        </div>
      ) : visible.length === 0 ? (
        <p className="px-1 py-3 text-xs text-muted-foreground">
          {tables.length === 0 ? "見られるテーブルがありません。" : "一致するテーブル・列がありません。"}
        </p>
      ) : (
        <ul className="min-h-0 space-y-0.5 overflow-y-auto pr-1 scrollbar-subtle">
          {visible.map((t) => {
            const searching = q !== "";
            const expanded = searching || open.has(t.id);
            const fields = [...t.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
            return (
              <li key={t.id} className="rounded-lg">
                <div className="flex items-center gap-1">
                  {searching ? (
                    <span className="flex size-6 shrink-0 items-center justify-center text-muted-foreground" aria-hidden>
                      <ChevronRight className="size-3.5 rotate-90" />
                    </span>
                  ) : (
                    <button
                      type="button"
                      onClick={() => toggle(t.id)}
                      aria-expanded={expanded}
                      aria-label={`「${t.name}」の列を${expanded ? "閉じる" : "開く"}`}
                      className="flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors duration-fast hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <ChevronRight className={cn("size-3.5 transition-transform duration-fast", expanded && "rotate-90")} aria-hidden />
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => onInsert(sqlIdent(t.name))}
                    className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-1.5 py-1 text-left text-[13px] font-medium transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    title={`「${t.name}」を入れる`}
                  >
                    <Table2 className="size-3.5 shrink-0 text-primary" aria-hidden />
                    <span className="truncate">{t.name}</span>
                    <span className="ml-auto shrink-0 text-[11px] font-normal tabular-nums text-muted-foreground">
                      {t.schema.fields.length} 列
                    </span>
                  </button>
                </div>
                {expanded ? (
                  <ul className="mb-1 ml-7 border-l pl-1.5">
                    {fields.map((f) => (
                      <FieldRow key={f.key} field={f} tableNames={tableNames} onInsert={onInsert} />
                    ))}
                    <li className="flex flex-wrap items-center gap-1 px-2 pt-1.5 text-[11px] text-muted-foreground">
                      システム列:
                      {["_owner", "_created_at", "_updated_at", "_id"].map((k) => (
                        <button
                          key={k}
                          type="button"
                          onClick={() => onInsert(k)}
                          className="rounded bg-muted px-1 font-mono text-[11px] transition-colors duration-fast hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                        >
                          {k}
                        </button>
                      ))}
                    </li>
                  </ul>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
