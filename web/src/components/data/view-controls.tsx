"use client";

/// グリッドのツールバーの操作（Task 15.5）: 絞り込み・並べ替え・列・グループ化・条件付き書式。
///
/// いずれも「今の表示」を変えるだけで、保存はビューの保存（不変の版の追記）で行う。並べ替え・絞り込みに
/// 使える列は索引が引ける列に限る（使えない列は選択肢に出さない）。未索引の列はサーバが自動で昇格し、
/// 大きなテーブルでは「準備中」になる（画面側が案内を出す）。

import {
  ArrowDown,
  ArrowDownUp,
  ArrowUp,
  Columns3,
  Eye,
  EyeOff,
  Filter,
  Group,
  Paintbrush,
  Pin,
  Plus,
  X,
} from "lucide-react";
import * as React from "react";

import { RefPicker } from "@/components/data/ref-picker";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import type { CondOp, ConditionalFormat, FieldDef, FormatColor, GroupConfig, SortKey } from "@/lib/data-api";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import {
  FORMAT_COLORS,
  isFilterable,
  isSortable,
  OP_LABELS,
  parseFilterValue,
  type ColumnModel,
  type FilterRow,
} from "@/lib/data-view";
import { cn } from "@/lib/utils";

export type { FilterRow };

const NO_VALUE_OPS: CondOp[] = ["exists", "is_null"];

function ToolbarButton({
  icon: Icon,
  label,
  count,
  active,
  ...props
}: React.ComponentProps<"button"> & { icon: React.ElementType; label: string; count?: number; active?: boolean }) {
  return (
    <Button
      variant="ghost"
      size="sm"
      className={cn("h-8 gap-1.5 px-2.5 text-[13px]", active && "bg-primary/10 text-primary hover:bg-primary/15")}
      {...props}
    >
      <Icon className="size-4" aria-hidden />
      <span className="max-sm:sr-only">{label}</span>
      {count ? (
        <span className="rounded-full bg-primary/15 px-1.5 text-[11px] font-semibold tabular-nums text-primary">
          {count}
        </span>
      ) : null}
    </Button>
  );
}

function FieldSelect({
  value,
  onChange,
  columns,
  label,
  className,
  currentLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  columns: ColumnModel[];
  label: string;
  className?: string;
  /// 選んである列が候補に無いとき（保存済みの条件の、いまは使えない列・テーブルに無い列）の名前。省略で参照名。
  currentLabel?: string;
}) {
  // 候補に無い列でも、選んである値は名前を出す（空欄にしない・BUG-244）。
  const unlisted = value !== "" && !columns.some((c) => c.key === value);
  return (
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger aria-label={label} className={cn("h-8 text-[13px]", className)}>
        <SelectValue placeholder="列を選ぶ" />
      </SelectTrigger>
      <SelectContent>
        {columns.map((c) => (
          <SelectItem key={c.key} value={c.key}>
            {c.label}
          </SelectItem>
        ))}
        {unlisted ? (
          <SelectItem value={value} className="text-muted-foreground">
            {currentLabel ?? value}
          </SelectItem>
        ) : null}
      </SelectContent>
    </Select>
  );
}

/// 演算子の候補（列の型と検索可否で絞る）。複数選択の「等しい」は「含む」の意味（要素のどれかが等しい）。
function opsFor(c: ColumnModel | undefined): CondOp[] {
  if (!c) return ["eq"];
  const t = c.field?.type;
  const rt = c.field?.result_type;
  const base: CondOp[] = ["eq", "neq"];
  const ordered =
    t === "number" || t === "date" || t === "date_time" || rt === "number" || rt === "date" || rt === "date_time" ||
    c.key === "_created_at" || c.key === "_updated_at";
  if (ordered) base.push("lt", "lte", "gt", "gte");
  if (t === "text") base.push("starts_with");
  // 部分一致・末尾一致は検索用の索引（searchable）を引く。
  if (c.field?.searchable) base.push("contains", "ends_with");
  base.push("exists", "is_null");
  return base;
}

/// 比べ方の表示（複数選択の「等しい」は「含む」と読める文言にする）。
function opLabel(c: ColumnModel | undefined, op: CondOp): string {
  if (c?.field?.type === "multi_select") {
    if (op === "eq") return "が次を含む";
    if (op === "neq") return "が次を含まない";
  }
  return OP_LABELS[op] ?? op;
}

/// ユーザー・部署の比較値（候補から 1 つ選ぶ・保存済みの値は表示名を引いて見せる）。
function FilterRefValue({
  field,
  value,
  onChange,
  label,
  className,
}: {
  field: FieldDef | { key: string; type: "user_ref" };
  value: string;
  onChange: (v: string) => void;
  label: string;
  className?: string;
}) {
  const [names, setNames] = React.useState<Record<string, string>>({});
  const id = React.useId().replace(/:/g, "");
  React.useEffect(() => {
    if (!value) return;
    let active = true;
    const done = (name: string | null | undefined) => active && name && setNames((n) => ({ ...n, [value]: name }));
    if (field.type === "user_ref") void resolveUserNames([value]).then(() => done(cachedUserName(value)));
    else void resolveRoleNames([value]).then(() => done(cachedRoleName(value)));
    return () => {
      active = false;
    };
  }, [field.type, value]);
  const [editing, setEditing] = React.useState(!value);
  const name = value ? names[value] || (field.type === "user_ref" ? cachedUserName(value) : cachedRoleName(value)) || value : "";
  if (value && !editing) {
    // 選んだ値は 1 行の欄に表示名で出す（押すと選び直せる）。行の高さを他の条件と揃える。
    return (
      <div className={cn("flex h-8 items-center rounded-md border border-input bg-background text-[13px]", className)} data-testid="filter-ref-value">
        <button
          type="button"
          aria-label={`${label}: ${name}（選び直す）`}
          onClick={() => setEditing(true)}
          className="min-w-0 flex-1 truncate rounded-l-md px-2.5 text-left hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {name}
        </button>
        <button
          type="button"
          aria-label={`${label}を外す`}
          onClick={() => {
            onChange("");
            setEditing(true);
          }}
          className="mr-1 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X className="size-3.5" aria-hidden />
        </button>
      </div>
    );
  }
  return (
    <div className={className} data-testid="filter-ref-value">
      <RefPicker
        field={field as FieldDef}
        value={[]}
        labels={names}
        onChange={(ids, labels) => {
          const id = ids[ids.length - 1] ?? "";
          if (id && labels[id]) setNames((n) => ({ ...n, [id]: labels[id] }));
          onChange(id);
          if (id) setEditing(false);
        }}
        id={`filter-${id}`}
        ariaLabel={label}
        autoFocus={Boolean(value)}
        listClassName="left-auto right-0 w-64"
      />
    </div>
  );
}

/// 比較値の入力欄（列の型ごとの部品）。
function ValueInput({
  col,
  value,
  onChange,
  onEnter,
  label,
  invalid,
  className,
}: {
  col: ColumnModel | undefined;
  value: string;
  onChange: (v: string) => void;
  onEnter?: () => void;
  label: string;
  invalid?: boolean;
  className?: string;
}) {
  const t = col?.field?.type;
  const rt = col?.field?.result_type;
  // 作成者（作成者の列・擬似列）もユーザーとして選ぶ。
  const refField =
    (t === "user_ref" || t === "role_ref") && col?.field
      ? col.field
      : t === "created_by" || col?.key === "_owner"
        ? ({ key: col?.key ?? "_owner", type: "user_ref" } as const)
        : null;
  if (refField) {
    // ユーザー・部署は候補から選ぶ（表示名を打って黙って 0 件にならない・比べる値は id・2-A-P2-3）。
    return <FilterRefValue field={refField} value={value} onChange={onChange} label={label} className={className} />;
  }
  if ((t === "select" || t === "multi_select") && col?.field) {
    return (
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger aria-label={label} className={cn("h-8 text-[13px]", className)}>
          <SelectValue placeholder="値を選ぶ" />
        </SelectTrigger>
        <SelectContent>
          {(col.field.options ?? []).map((o) => (
            <SelectItem key={o} value={o}>
              {o}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    );
  }
  const kind =
    t === "date" || rt === "date"
      ? "date"
      : t === "date_time" || rt === "date_time" || col?.key === "_created_at" || col?.key === "_updated_at"
        ? "datetime-local"
        : "text";
  const numeric = t === "number" || rt === "number";
  return (
    <Input
      aria-label={label}
      aria-invalid={invalid || undefined}
      type={kind}
      inputMode={numeric ? "decimal" : undefined}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => e.key === "Enter" && onEnter?.()}
      placeholder={numeric ? "数値" : "値"}
      className={cn("h-8 text-[13px]", invalid && "border-destructive focus-visible:ring-destructive/40", className)}
    />
  );
}

/// 絞り込みの部品の文言（呼び出し側の画面の言葉で言う・付録 D の N3.3.1）。
export type FilterTexts = {
  /// ツールバーのボタンとポップオーバーの題。
  title: string;
  /// 題の横の補足。
  subtitle: string;
  /// 画面で編集できない条件があるときの文と、それを外すボタン。
  advanced: string;
  clearAdvanced: string;
  /// 条件が無いとき。
  empty: string;
  /// 適用のボタン。
  apply: string;
};

const DATA_FILTER_TEXTS: FilterTexts = {
  title: "絞り込み",
  subtitle: "すべての条件を満たす行",
  advanced: "ビューに保存された詳細な条件が掛かっています",
  clearAdvanced: "外す",
  empty: "条件はありません。「条件を追加」で足せます。",
  apply: "適用",
};

/// 比べる値を式で決める部品（ワークフローの「探す条件」の「入力や結果から選ぶ」・BUG-98）。
export type FilterExprValue = {
  /// 式を選ぶ欄（`value` は選んだ式・まだ選んでいなければ null）。
  render: (p: {
    label: string;
    value: Record<string, unknown> | null;
    onChange: (next: Record<string, unknown>) => void;
  }) => React.ReactNode;
};

/// 比べる値の入れ方の切り替え（直接入力｜入力や結果から選ぶ・付録 D の N1.5.1）。
///
/// ARIA の radiogroup の作り: 選んでいる方だけが Tab で止まり（roving tabindex）、矢印キーで選び直す。
function ValueModeToggle({
  label,
  expr,
  onChange,
}: {
  label: string;
  expr: boolean;
  onChange: (expr: boolean) => void;
}) {
  const refs = React.useRef<(HTMLButtonElement | null)[]>([]);
  const choose = (on: boolean) => {
    if (expr !== on) onChange(on);
    refs.current[on ? 1 : 0]?.focus();
  };
  const item = (on: boolean, text: string) => (
    <button
      type="button"
      role="radio"
      ref={(el) => {
        refs.current[on ? 1 : 0] = el;
      }}
      aria-checked={expr === on}
      tabIndex={expr === on ? 0 : -1}
      onClick={() => expr !== on && onChange(on)}
      onKeyDown={(e) => {
        if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) {
          e.preventDefault();
          choose(!on);
        }
      }}
      className={cn(
        "rounded px-1.5 py-0.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:py-1.5",
        expr === on ? "bg-primary/10 font-medium text-primary" : "text-muted-foreground hover:text-foreground",
      )}
    >
      {text}
    </button>
  );
  return (
    <div
      role="radiogroup"
      aria-label={`${label}の入れ方`}
      className="mb-1 flex flex-wrap gap-0.5 whitespace-nowrap text-[11px]"
      data-testid="filter-value-mode"
    >
      {item(false, "直接入力")}
      {item(true, "入力や結果から選ぶ")}
    </div>
  );
}

export function FilterControl({
  columns,
  rows,
  onChange,
  advanced,
  onClearAdvanced,
  texts,
  exprValue,
  usable,
  columnError,
}: {
  columns: ColumnModel[];
  rows: FilterRow[];
  onChange: (rows: FilterRow[]) => void;
  /// 保存ビューの条件が画面で編集できない形（OR・入れ子）のとき true。
  advanced: boolean;
  onClearAdvanced: () => void;
  /// 文言（省略でデータの画面の言葉）。
  texts?: Partial<FilterTexts>;
  /// 比べる値を式でも決められるようにする（省略で直接入力だけ）。
  exprValue?: FilterExprValue;
  /// 列を比べ方に使えるかで、選べる列と比べ方をさらに絞る（ワークフローの探す条件は索引のある列と、部分一致で検索
  /// できる列の「含む」「で終わる」だけ・BUG-244）。使える比べ方が 1 つも無い列は選ばせない。省略で絞り込める列すべて。
  usable?: (c: ColumnModel, op: CondOp) => boolean;
  /// 列を断った保存時の誤り（列の参照名 → 文言）。その列の条件の下に出す。
  columnError?: (key: string) => string | null;
}) {
  const t = { ...DATA_FILTER_TEXTS, ...texts };
  // 列に使える比べ方（`usable` で絞る）。
  const opsOf = (c: ColumnModel | undefined): CondOp[] =>
    c && usable ? opsFor(c).filter((op) => usable(c, op)) : opsFor(c);
  const filterable = columns.filter((c) => isFilterable(c) && opsOf(c).length > 0);
  const [draft, setDraft] = React.useState<FilterRow[]>(rows);
  const [open, setOpen] = React.useState(false);
  React.useEffect(() => {
    if (open) setDraft(rows.length ? rows : []);
  }, [open, rows]);
  const set = (i: number, patch: Partial<FilterRow>) =>
    setDraft((d) => d.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  // 型に合わない値（数値の列に文字・日付でない文字列）は送らない（A-9）。空の値の条件は適用時に捨てる。
  // 「入力や結果から選ぶ」にして値を選んでいない条件は、黙って捨てずに選ぶよう促す。
  const errorOf = (r: FilterRow): string | null => {
    if (!r.field || NO_VALUE_OPS.includes(r.op)) return null;
    if (r.expr !== undefined) return r.expr === null ? "値を選んでください" : null;
    if (r.value.trim() === "") return null;
    const v = parseFilterValue(r.value, columns.find((c) => c.key === r.field), r.op);
    return v.ok ? null : v.error;
  };
  const hasError = draft.some((r) => errorOf(r) !== null);
  const apply = () => {
    if (hasError) return;
    onChange(
      draft.filter((r) => r.field && (NO_VALUE_OPS.includes(r.op) || r.expr !== undefined || r.value.trim() !== "")),
    );
    setOpen(false);
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <ToolbarButton
          icon={Filter}
          label={t.title}
          count={rows.length + (advanced ? 1 : 0)}
          active={rows.length > 0 || advanced}
          data-testid="data-filter-button"
        />
      </PopoverTrigger>
      {/* 条件が多いとき・狭い画面では、画面の端に付けず、はみ出す分は中で送る（適用のボタンが画面の外に出ない）。 */}
      <PopoverContent
        align="start"
        collisionPadding={12}
        className="max-h-[var(--radix-popover-content-available-height)] w-[min(30rem,calc(100vw-1.5rem))] space-y-3 overflow-y-auto"
      >
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">{t.title}</h3>
          <span className="text-[11px] text-muted-foreground">{t.subtitle}</span>
        </div>
        {advanced ? (
          <div className="flex items-center justify-between gap-2 rounded-md border border-dashed px-2.5 py-2 text-xs text-muted-foreground">
            {t.advanced}
            <Button variant="ghost" size="sm" className="h-7 shrink-0 text-xs" onClick={onClearAdvanced}>
              {t.clearAdvanced}
            </Button>
          </div>
        ) : null}
        {draft.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t.empty}</p>
        ) : (
          <ul className="space-y-2">
            {draft.map((r, i) => {
              const col = columns.find((c) => c.key === r.field);
              // 使えない比べ方で保存した条件も、選んだ比べ方の名前のまま出す（空欄にしない）。
              const allowed = opsOf(col);
              const ops = allowed.includes(r.op) ? allowed : [...allowed, r.op];
              const err = errorOf(r);
              const fieldErr = r.field ? (columnError?.(r.field) ?? null) : null;
              return (
                <li
                  key={i}
                  className={cn(
                    "grid grid-cols-[1fr_1fr_auto] items-center gap-1.5",
                    // 値の入れ方を選べるときは、値の欄を列と比べ方の下に広く取る（狭い列に押し込まない）。
                    exprValue ? "border-b pb-2 last:border-b-0 last:pb-0" : "sm:grid-cols-[9rem_9rem_1fr_auto]",
                  )}
                >
                  <FieldSelect
                    label={`条件 ${i + 1} の列`}
                    value={r.field}
                    columns={filterable}
                    currentLabel={col?.label}
                    // 列を変えたら比べ方と値は列の型に合わせて作り直す（前の列の値を持ち越さない）。入れ方
                    // （入力や結果から選ぶ）は残し、選んだ値だけを外す。
                    onChange={(field) =>
                      set(i, {
                        field,
                        op: opsOf(columns.find((c) => c.key === field))[0],
                        value: "",
                        expr: r.expr === undefined ? undefined : null,
                      })
                    }
                  />
                  <Select value={r.op} onValueChange={(op) => set(i, { op: op as CondOp })}>
                    <SelectTrigger aria-label={`条件 ${i + 1} の比べ方`} className="h-8 text-[13px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {ops.map((o) => (
                        <SelectItem key={o} value={o} className={allowed.includes(o) ? undefined : "text-muted-foreground"}>
                          {opLabel(col, o)}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <div
                    className={cn(
                      "col-span-2 row-start-2 min-w-0",
                      !exprValue && "sm:col-span-1 sm:row-start-auto",
                    )}
                  >
                    {NO_VALUE_OPS.includes(r.op) ? null : (
                      <>
                        {exprValue ? (
                          <ValueModeToggle
                            label={`条件 ${i + 1} の値`}
                            expr={r.expr !== undefined}
                            onChange={(on) => set(i, on ? { expr: null, value: "" } : { expr: undefined, value: "" })}
                          />
                        ) : null}
                        {exprValue && r.expr !== undefined ? (
                          exprValue.render({
                            label: `条件 ${i + 1} の値`,
                            value: r.expr,
                            onChange: (expr) => set(i, { expr }),
                          })
                        ) : (
                          <ValueInput
                            col={col}
                            label={`条件 ${i + 1} の値`}
                            value={r.value}
                            invalid={err !== null}
                            onChange={(value) => set(i, { value })}
                            onEnter={apply}
                          />
                        )}
                      </>
                    )}
                    {err ? (
                      <p className="mt-1 text-[11px] text-destructive" role="alert">
                        {err}
                      </p>
                    ) : null}
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-8"
                    aria-label={`条件 ${i + 1} を外す`}
                    onClick={() => setDraft((d) => d.filter((_, j) => j !== i))}
                  >
                    <X className="size-4" aria-hidden />
                  </Button>
                  {/* 外すボタンの後に置く（グリッドの自動配置で、外すボタンが誤りの下の段へ回らない）。 */}
                  {fieldErr ? (
                    <p className="col-span-full text-[11px] text-destructive" data-testid="filter-column-error">
                      {fieldErr}
                    </p>
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}
        <div className="flex items-center justify-between gap-2 border-t pt-3">
          <Button
            variant="ghost"
            size="sm"
            className="h-8 text-xs"
            disabled={filterable.length === 0}
            onClick={() =>
              setDraft((d) => [...d, { field: filterable[0]?.key ?? "", op: opsOf(filterable[0])[0], value: "" }])
            }
          >
            <Plus className="size-3.5" aria-hidden />
            条件を追加
          </Button>
          <div className="flex gap-2">
            {rows.length > 0 ? (
              <Button
                variant="outline"
                size="sm"
                className="h-8 text-xs"
                onClick={() => {
                  onChange([]);
                  setOpen(false);
                }}
              >
                すべて外す
              </Button>
            ) : null}
            <Button size="sm" className="h-8 text-xs" onClick={apply} disabled={hasError} data-testid="data-filter-apply">
              {t.apply}
            </Button>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function SortControl({
  columns,
  sort,
  maxKeys,
  onChange,
  usable,
  columnError,
}: {
  columns: ColumnModel[];
  sort: SortKey[];
  maxKeys: number;
  onChange: (sort: SortKey[]) => void;
  /// 選べる列をさらに絞る（ワークフローの並べ替えは索引のある列だけ・BUG-244）。省略で並べ替えできる列すべて。
  usable?: (c: ColumnModel) => boolean;
  /// 列を断った保存時の誤り（列の参照名 → 文言）。その列の並べ替えの下に出す。
  columnError?: (key: string) => string | null;
}) {
  const sortable = columns.filter((c) => isSortable(c) && (usable?.(c) ?? true));
  // 足せる列が残っていなければ「並べ替えを追加」を押せなくする（押しても何も起きないボタンにしない）。
  const nextKey = sortable.find((c) => !sort.some((s) => s.field === c.key));
  return (
    <Popover>
      <PopoverTrigger asChild>
        <ToolbarButton icon={ArrowDownUp} label="並べ替え" count={sort.length} active={sort.length > 0} />
      </PopoverTrigger>
      {/* 画面の端に付けない（探す条件と同じ）。 */}
      <PopoverContent align="start" collisionPadding={12} className="w-80 space-y-3">
        <h3 className="text-sm font-semibold">並べ替え</h3>
        {sort.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            既定は更新の新しい順です。複数選択・リンク・索引のない派生列は並べ替えに使えません。
          </p>
        ) : (
          <ol className="space-y-2">
            {sort.map((k, i) => {
              const fieldErr = columnError?.(k.field) ?? null;
              return (
                <li key={`${k.field}:${i}`} className="space-y-1">
                  <div className="flex items-center gap-1.5">
                    <span className="w-4 text-center text-[11px] tabular-nums text-muted-foreground">{i + 1}</span>
                    <FieldSelect
                      label={`並べ替え ${i + 1} の列`}
                      value={k.field}
                      columns={sortable.filter((c) => c.key === k.field || !sort.some((s) => s.field === c.key))}
                      currentLabel={columns.find((c) => c.key === k.field)?.label}
                      onChange={(field) => onChange(sort.map((s, j) => (j === i ? { ...s, field } : s)))}
                      className="flex-1"
                    />
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-8 w-20 gap-1 px-2 text-xs"
                      onClick={() => onChange(sort.map((s, j) => (j === i ? { ...s, descending: !s.descending } : s)))}
                    >
                      {k.descending ? <ArrowDown className="size-3.5" aria-hidden /> : <ArrowUp className="size-3.5" aria-hidden />}
                      {k.descending ? "降順" : "昇順"}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-8"
                      aria-label={`並べ替え ${i + 1} を外す`}
                      onClick={() => onChange(sort.filter((_, j) => j !== i))}
                    >
                      <X className="size-4" aria-hidden />
                    </Button>
                  </div>
                  {fieldErr ? (
                    <p className="pl-5 text-[11px] text-destructive" data-testid="sort-column-error">
                      {fieldErr}
                    </p>
                  ) : null}
                </li>
              );
            })}
          </ol>
        )}
        <Button
          variant="ghost"
          size="sm"
          className="h-8 text-xs"
          disabled={sort.length >= maxKeys || !nextKey}
          onClick={() => {
            if (nextKey) onChange([...sort, { field: nextKey.key, descending: false }]);
          }}
        >
          <Plus className="size-3.5" aria-hidden />
          並べ替えを追加
          {sort.length >= maxKeys ? `（最大 ${maxKeys}）` : ""}
        </Button>
      </PopoverContent>
    </Popover>
  );
}

/// 列の表示（順・見出し・非表示・固定）。ビューごとの設定で、テーブルの表示名は変えない。
export function ColumnsControl({
  columns,
  onChange,
}: {
  columns: ColumnModel[];
  onChange: (columns: ColumnModel[]) => void;
}) {
  // 擬似列（作成日時など）は既定で隠れているので数えない（ユーザーが隠した列だけを示す）。
  const hidden = columns.filter((c) => c.hidden && !c.system).length;
  const move = (i: number, d: -1 | 1) => {
    const j = i + d;
    if (j < 0 || j >= columns.length) return;
    const next = [...columns];
    [next[i], next[j]] = [next[j], next[i]];
    onChange(next);
  };
  const patch = (i: number, p: Partial<ColumnModel>) => onChange(columns.map((c, j) => (j === i ? { ...c, ...p } : c)));
  // 固定は左端からの連続だけ（グリッドは先頭から連続した固定列しか描けず、飛び石の固定はビューの保存で拒否
  // される）。固定するとその列までの表示中の列をまとめて固定し、外すとその列から右の固定をまとめて外す。
  const toggleFreeze = (i: number) => {
    const on = !columns[i].frozen;
    onChange(
      columns.map((c, j) => {
        if (c.hidden) return { ...c, frozen: false };
        if (on) return j <= i ? { ...c, frozen: true } : c;
        return j >= i ? { ...c, frozen: false } : c;
      }),
    );
  };
  return (
    <Popover>
      <PopoverTrigger asChild>
        <ToolbarButton
          icon={Columns3}
          label="列"
          count={hidden || undefined}
          active={hidden > 0}
          data-testid="data-columns-button"
        />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[min(24rem,calc(100vw-1.5rem))] p-0">
        <div className="flex items-center justify-between border-b px-3 py-2.5">
          <h3 className="text-sm font-semibold">列の表示</h3>
          <span className="text-[11px] text-muted-foreground">このビューだけに効きます</span>
        </div>
        <ul className="max-h-[min(26rem,60dvh)] space-y-0.5 overflow-y-auto p-1.5 scrollbar-subtle">
          {columns.map((c, i) => (
            <li
              key={c.key}
              className={cn("group/col flex items-center gap-1 rounded-md px-1.5 py-1 hover:bg-accent/60", c.hidden && "opacity-60")}
            >
              <div className="flex flex-col">
                <button
                  type="button"
                  aria-label={`「${c.label}」を上へ`}
                  disabled={i === 0}
                  onClick={() => move(i, -1)}
                  className="rounded px-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <ArrowUp className="size-3" aria-hidden />
                </button>
                <button
                  type="button"
                  aria-label={`「${c.label}」を下へ`}
                  disabled={i === columns.length - 1}
                  onClick={() => move(i, 1)}
                  className="rounded px-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <ArrowDown className="size-3" aria-hidden />
                </button>
              </div>
              <Input
                aria-label={`「${c.baseLabel}」の見出し`}
                value={c.label}
                onChange={(e) => patch(i, { label: e.target.value })}
                onBlur={(e) => !e.target.value.trim() && patch(i, { label: c.baseLabel })}
                maxLength={100}
                className="h-7 flex-1 border-transparent bg-transparent px-1.5 text-[13px] shadow-none hover:border-border focus-visible:border-border"
              />
              {c.label !== c.baseLabel ? (
                <span className="hidden max-w-20 truncate text-[10px] text-muted-foreground sm:inline" title={c.baseLabel}>
                  {c.baseLabel}
                </span>
              ) : null}
              <Button
                variant="ghost"
                size="icon"
                className={cn("size-7", c.frozen ? "text-primary" : "text-muted-foreground")}
                aria-label={c.frozen ? `「${c.label}」の固定を外す` : `「${c.label}」まで左に固定`}
                title={c.frozen ? "この列から右の固定を外す" : "左端からこの列までを固定"}
                aria-pressed={c.frozen}
                disabled={c.hidden}
                onClick={() => toggleFreeze(i)}
              >
                <Pin className={cn("size-3.5", c.frozen && "fill-current")} aria-hidden />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                className="size-7 text-muted-foreground"
                aria-label={c.hidden ? `「${c.label}」を表示` : `「${c.label}」を隠す`}
                aria-pressed={!c.hidden}
                onClick={() => patch(i, { hidden: !c.hidden, frozen: c.hidden ? c.frozen : false })}
              >
                {c.hidden ? <EyeOff className="size-3.5" aria-hidden /> : <Eye className="size-3.5" aria-hidden />}
              </Button>
            </li>
          ))}
        </ul>
      </PopoverContent>
    </Popover>
  );
}

export function GroupControl({
  columns,
  groups,
  onChange,
}: {
  columns: ColumnModel[];
  groups: GroupConfig[];
  onChange: (g: GroupConfig[]) => void;
}) {
  const groupable = columns.filter(isSortable);
  return (
    <Popover>
      <PopoverTrigger asChild>
        <ToolbarButton icon={Group} label="グループ" count={groups.length} active={groups.length > 0} />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 space-y-3">
        <h3 className="text-sm font-semibold">行のグループ化</h3>
        {groups.length === 0 ? (
          <p className="text-xs text-muted-foreground">列の値ごとに見出しで区切ります（最大 3 段）。</p>
        ) : (
          <ol className="space-y-2">
            {groups.map((g, i) => (
              <li key={`${g.field}:${i}`} className="flex items-center gap-1.5">
                <FieldSelect
                  label={`グループ ${i + 1} の列`}
                  value={g.field}
                  columns={groupable.filter((c) => c.key === g.field || !groups.some((x) => x.field === c.key))}
                  onChange={(field) => onChange(groups.map((x, j) => (j === i ? { ...x, field } : x)))}
                  className="flex-1"
                />
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 w-20 gap-1 px-2 text-xs"
                  onClick={() => onChange(groups.map((x, j) => (j === i ? { ...x, descending: !x.descending } : x)))}
                >
                  {g.descending ? "降順" : "昇順"}
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-8"
                  aria-label={`グループ ${i + 1} を外す`}
                  onClick={() => onChange(groups.filter((_, j) => j !== i))}
                >
                  <X className="size-4" aria-hidden />
                </Button>
              </li>
            ))}
          </ol>
        )}
        <Button
          variant="ghost"
          size="sm"
          className="h-8 text-xs"
          disabled={groups.length >= 3}
          onClick={() => {
            const next = groupable.find((c) => !groups.some((g) => g.field === c.key));
            if (next) onChange([...groups, { field: next.key }]);
          }}
        >
          <Plus className="size-3.5" aria-hidden />
          グループを追加
        </Button>
      </PopoverContent>
    </Popover>
  );
}

/// 条件付き書式（列の値で行・セルを塗る）。条件は「列 比べ方 値」1 つずつ（保存形は閉じた条件木）。
export function FormatControl({
  columns,
  formats,
  onChange,
}: {
  columns: ColumnModel[];
  formats: ConditionalFormat[];
  onChange: (f: ConditionalFormat[]) => void;
}) {
  const visible = columns.filter((c) => !c.hidden);
  const rowOf = (f: ConditionalFormat): FilterRow | null =>
    "cmp" in f.condition
      ? { field: f.condition.cmp.field, op: f.condition.cmp.op, value: String(f.condition.cmp.value ?? "") }
      : null;
  const toFormat = (r: FilterRow, color: FormatColor, prev?: ConditionalFormat): ConditionalFormat => {
    const col = columns.find((c) => c.key === r.field);
    const numeric = col?.field?.type === "number";
    const value = NO_VALUE_OPS.includes(r.op) ? undefined : numeric && r.value !== "" ? Number(r.value) : r.value;
    return { ...prev, condition: { cmp: { field: r.field, op: r.op, value } }, color };
  };
  return (
    <Popover>
      <PopoverTrigger asChild>
        <ToolbarButton icon={Paintbrush} label="書式" count={formats.length} active={formats.length > 0} />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[min(30rem,calc(100vw-1.5rem))] space-y-3">
        <h3 className="text-sm font-semibold">条件付き書式</h3>
        {formats.length === 0 ? (
          <p className="text-xs text-muted-foreground">条件に合う行に色を付けます（上の規則が優先）。</p>
        ) : (
          <ul className="space-y-2">
            {formats.map((f, i) => {
              const r = rowOf(f);
              if (!r) {
                return (
                  <li key={i} className="flex items-center justify-between rounded-md border border-dashed px-2.5 py-1.5 text-xs text-muted-foreground">
                    詳細な条件の規則
                    <Button variant="ghost" size="sm" className="h-7 text-xs" onClick={() => onChange(formats.filter((_, j) => j !== i))}>
                      外す
                    </Button>
                  </li>
                );
              }
              const col = columns.find((c) => c.key === r.field);
              const upd = (p: Partial<FilterRow>) => onChange(formats.map((x, j) => (j === i ? toFormat({ ...r, ...p }, x.color, x) : x)));
              return (
                <li key={i} className="grid grid-cols-[1fr_1fr_auto_auto] items-center gap-1.5">
                  <FieldSelect label={`規則 ${i + 1} の列`} value={r.field} columns={visible} onChange={(field) => upd({ field })} />
                  <Select value={r.op} onValueChange={(op) => upd({ op: op as CondOp })}>
                    <SelectTrigger aria-label={`規則 ${i + 1} の比べ方`} className="h-8 text-[13px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {opsFor(col).map((o) => (
                        <SelectItem key={o} value={o}>
                          {OP_LABELS[o] ?? o}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Select value={f.color} onValueChange={(c) => onChange(formats.map((x, j) => (j === i ? { ...x, color: c as FormatColor } : x)))}>
                    <SelectTrigger aria-label={`規則 ${i + 1} の色`} className="h-8 w-20 text-[13px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {(Object.keys(FORMAT_COLORS) as FormatColor[]).map((c) => (
                        <SelectItem key={c} value={c}>
                          <span className="inline-flex items-center gap-1.5">
                            <span className="size-3 rounded-sm border" style={{ background: FORMAT_COLORS[c].bg }} />
                            {FORMAT_COLORS[c].label}
                          </span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Button variant="ghost" size="icon" className="size-8" aria-label={`規則 ${i + 1} を外す`} onClick={() => onChange(formats.filter((_, j) => j !== i))}>
                    <X className="size-4" aria-hidden />
                  </Button>
                  {NO_VALUE_OPS.includes(r.op) ? null : (
                    <Input
                      aria-label={`規則 ${i + 1} の値`}
                      value={r.value}
                      onChange={(e) => upd({ value: e.target.value })}
                      placeholder="値"
                      className="col-span-4 h-8 text-[13px]"
                    />
                  )}
                </li>
              );
            })}
          </ul>
        )}
        <Button
          variant="ghost"
          size="sm"
          className="h-8 text-xs"
          disabled={visible.length === 0 || formats.length >= 32}
          onClick={() => onChange([...formats, toFormat({ field: visible[0].key, op: "is_null", value: "" }, "yellow")])}
        >
          <Plus className="size-3.5" aria-hidden />
          規則を追加
        </Button>
      </PopoverContent>
    </Popover>
  );
}
