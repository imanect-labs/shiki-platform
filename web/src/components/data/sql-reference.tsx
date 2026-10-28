"use client";

/// SQL 画面の参照一覧（#978）: 閲覧者に見えるテーブルと列。押すと入力欄のカーソル位置へ名前を入れる。
///
/// 一覧は `GET /data/tables`（閲覧者に読める列だけに絞った定義）なので、ここに出る名前は SQL に書いて
/// 通るものだけ。見えないテーブル・列は出ない（存在を明かさない）。

import { ChevronRight, Link2, Search, Table2 } from "lucide-react";
import * as React from "react";

import { Input } from "@/components/ui/input";
import type { DataTable, FieldDef } from "@/lib/data-api";
import { cn } from "@/lib/utils";

/// SQL に書けるテーブル名（識別子として書けない文字を含むなら二重引用符で囲む）。
export function sqlTableName(name: string): string {
  return /^[\p{L}_][\p{L}\p{N}_]*$/u.test(name) ? name : `"${name.replaceAll('"', '""')}"`;
}

const TYPE_LABEL: Record<string, string> = {
  text: "テキスト",
  number: "数値",
  date: "日付",
  date_time: "日時",
  select: "選択",
  multi_select: "複数選択",
  user_ref: "ユーザー",
  role_ref: "部署",
  record_ref: "リンク",
  file_ref: "ファイル",
  formula: "数式",
  lookup: "ルックアップ",
  rollup: "ロールアップ",
  computed: "計算",
};

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
  return (
    <li>
      <button
        type="button"
        onClick={() => onInsert(field.key)}
        className="group/field flex w-full min-w-0 items-baseline gap-2 rounded-md px-2 py-1 text-left transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        title={`「${field.key}」を入れる`}
      >
        <code className="shrink-0 font-mono text-[12.5px] text-foreground">{field.key}</code>
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {field.display_name && field.display_name !== field.key ? `${field.display_name} · ` : ""}
          {TYPE_LABEL[field.type] ?? field.type}
          {target ? (
            <>
              {" "}
              <Link2 className="inline size-3 align-[-1px]" aria-hidden />
              {target}
            </>
          ) : null}
        </span>
      </button>
    </li>
  );
}

export function SqlReference({
  tables,
  onInsert,
  className,
}: {
  tables: DataTable[] | null;
  onInsert: (text: string) => void;
  className?: string;
}) {
  const [query, setQuery] = React.useState("");
  const [open, setOpen] = React.useState<Set<string>>(new Set());
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
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-sm font-semibold">テーブルと列</h2>
        <span className="text-[11px] text-muted-foreground">押すと入力欄に入ります</span>
      </div>
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
      ) : visible.length === 0 ? (
        <p className="px-1 py-3 text-xs text-muted-foreground">
          {tables.length === 0 ? "見られるテーブルがありません。" : "一致するテーブル・列がありません。"}
        </p>
      ) : (
        <ul className="min-h-0 space-y-0.5 overflow-y-auto pr-1 scrollbar-subtle">
          {visible.map((t) => {
            const expanded = open.has(t.id) || q !== "";
            const fields = [...t.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
            return (
              <li key={t.id} className="rounded-lg">
                <div className="flex items-center gap-1">
                  <button
                    type="button"
                    onClick={() => toggle(t.id)}
                    aria-expanded={expanded}
                    aria-label={`「${t.name}」の列を${expanded ? "閉じる" : "開く"}`}
                    className="flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors duration-fast hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <ChevronRight className={cn("size-3.5 transition-transform duration-fast", expanded && "rotate-90")} aria-hidden />
                  </button>
                  <button
                    type="button"
                    onClick={() => onInsert(sqlTableName(t.name))}
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
                    <li className="px-2 pt-1 text-[11px] leading-5 text-muted-foreground">
                      システム列: <code className="font-mono">_owner</code> <code className="font-mono">_created_at</code>{" "}
                      <code className="font-mono">_updated_at</code> <code className="font-mono">_id</code>
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
