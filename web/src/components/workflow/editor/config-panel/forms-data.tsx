"use client";

/// 業務データ（data.*）ノードのフォーム（Task 15.7 のノード・設定画面は Task 15.5）。
///
/// 対象テーブルは一覧から選ぶ（固定値の UUID）。固定値で選ぶと、有効化のときに渡す権限
/// （data_table の viewer / editor）の対象がそのテーブルに決まる。前の結果から渡す指定も残す。
/// 絞り込み・並べ替えはデータ画面と同じ部品で、保存形は data の `DataQuery`（IR をそのまま渡す）。

import * as React from "react";

import { FilterControl, SortControl } from "@/components/data/view-controls";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import { listTables, type DataQuery, type DataTable } from "@/lib/data-api";
import { buildFilter, resolveColumns, splitFilter } from "@/lib/data-view";
import { Field, NumberInput } from "./common";
import type { FormProps } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

/// 自分が使えるテーブルの一覧（パネルを開くたびに 1 回だけ読む・`enabled` が偽の間は読まない）。
export function useDataTables(enabled = true): { tables: DataTable[] | null; error: string | null } {
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    if (!enabled) return;
    let active = true;
    listTables()
      .then((t) => active && setTables(t))
      .catch((e) => {
        if (!active) return;
        setTables([]);
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [enabled]);
  return { tables, error };
}

/// テーブルの選択（一覧から固定値で選ぶ）。
export function DataTableSelect({
  value,
  onChange,
  tables,
  label = "対象のテーブル",
  id,
}: {
  value: string | null;
  onChange: (id: string) => void;
  tables: DataTable[] | null;
  label?: string;
  id?: string;
}) {
  return (
    <Select value={value ?? ""} onValueChange={onChange} disabled={tables === null}>
      <SelectTrigger id={id} aria-label={label} className="h-8 text-[13px]">
        <SelectValue placeholder={tables === null ? "読み込み中…" : "テーブルを選ぶ"} />
      </SelectTrigger>
      <SelectContent>
        {(tables ?? []).map((t) => (
          <SelectItem key={t.id} value={t.id}>
            {t.name}
          </SelectItem>
        ))}
        {tables && tables.length === 0 ? (
          <div className="px-2 py-1.5 text-xs text-muted-foreground">使えるテーブルがありません</div>
        ) : null}
      </SelectContent>
    </Select>
  );
}

/// 対象テーブル（固定値の選択 ／ 前の結果から）。選んだテーブルを返す（列の候補に使う）。
function TableField({ node, dispatch, refCandidates, inMapRegion, fieldErrors, tables }: FormProps & { tables: DataTable[] | null }) {
  const p = paramsOf<{ table: ValueExpr }>(node);
  const literal = typeof p.table === "string" ? p.table : null;
  const dynamic = p.table !== undefined && literal === null;
  const [mode, setMode] = React.useState<"pick" | "expr">(dynamic ? "expr" : "pick");
  return (
    <div className="space-y-1.5">
      {mode === "pick" ? (
        <Field label="対象のテーブル" error={fieldErrors.get("table")} hint="有効化のとき、このテーブルの権限を渡します">
          <DataTableSelect value={literal} tables={tables} onChange={(id) => patchParams(dispatch, node, { table: id })} />
        </Field>
      ) : (
        <ValueExprInput
          label="対象のテーブル"
          value={p.table}
          onChange={(v) => patchParams(dispatch, node, { table: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="テーブル ID"
          error={fieldErrors.get("table")}
        />
      )}
      <button
        type="button"
        className="text-[11px] text-primary underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() => setMode(mode === "pick" ? "expr" : "pick")}
      >
        {mode === "pick" ? "前の結果から指定する" : "一覧から選ぶ"}
      </button>
    </div>
  );
}

function useTableOf(node: FormProps["node"], tables: DataTable[] | null): DataTable | null {
  const p = paramsOf<{ table: ValueExpr }>(node);
  return typeof p.table === "string" ? ((tables ?? []).find((t) => t.id === p.table) ?? null) : null;
}

function FieldKeysHint({ table }: { table: DataTable | null }) {
  if (!table) return null;
  return (
    <p className="text-[11px] leading-relaxed text-muted-foreground">
      列の参照名:{" "}
      {table.schema.fields.map((f, i) => (
        <span key={f.key}>
          {i > 0 ? "、" : ""}
          <code className="font-mono">{f.key}</code>
          {f.display_name && f.display_name !== f.key ? `（${f.display_name}）` : ""}
        </span>
      ))}
    </p>
  );
}

export function DataQueryForm(props: FormProps) {
  const { node, dispatch, fieldErrors } = props;
  const { tables } = useDataTables();
  const table = useTableOf(node, tables);
  const p = paramsOf<NodeParamsByType["data.query"]>(node);
  const query: DataQuery = (p.query as DataQuery | undefined) ?? {};
  const columns = React.useMemo(() => (table ? resolveColumns(table.schema.fields, null, new Set()) : []), [table]);
  const f = splitFilter(query.filter);
  const setQuery = (next: DataQuery) => {
    const cleaned = Object.fromEntries(Object.entries(next).filter(([, v]) => v !== undefined && v !== null));
    patchParams(dispatch, node, { query: Object.keys(cleaned).length ? cleaned : undefined });
  };
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      {table ? (
        <div className="space-y-1.5">
          <span className="text-xs font-medium">絞り込み・並べ替え</span>
          <div className="flex flex-wrap gap-1 rounded-md border p-1">
            <FilterControl
              columns={columns}
              rows={f.rows}
              advanced={f.advanced !== null}
              onClearAdvanced={() => setQuery({ ...query, filter: buildFilter(f.rows, null, columns) ?? undefined })}
              onChange={(rows) => setQuery({ ...query, filter: buildFilter(rows, f.advanced, columns) ?? undefined })}
            />
            <SortControl
              columns={columns}
              sort={query.sort ?? []}
              maxKeys={4}
              onChange={(sort) => setQuery({ ...query, sort: sort.length ? sort : undefined })}
            />
          </div>
          {fieldErrors.get("query") ? <p className="text-[11px] text-destructive">{fieldErrors.get("query")}</p> : null}
        </div>
      ) : null}
      <Field label="取得件数（省略可）" hint="空欄で 50 件・最大 200 件">
        <NumberInput
          value={typeof query.limit === "number" ? query.limit : null}
          min={1}
          max={200}
          onChange={(n) => setQuery({ ...query, limit: n ?? undefined })}
        />
      </Field>
    </div>
  );
}

export function DataRecordCreateForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { tables } = useDataTables();
  const table = useTableOf(node, tables);
  const p = paramsOf<NodeParamsByType["data.record.create"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      <ValueExprInput
        label="レコードの値"
        value={p.data as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { data: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="object"
        placeholder='例: {"name": "出張旅費", "amount": 12000}'
        error={fieldErrors.get("data")}
      />
      <FieldKeysHint table={table} />
    </div>
  );
}

function RecordAndRev(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const p = paramsOf<{ record?: ValueExpr; expected_rev?: ValueExpr }>(node);
  return (
    <>
      <ValueExprInput
        label="対象のレコード"
        value={p.record}
        onChange={(v) => patchParams(dispatch, node, { record: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="レコード ID"
        error={fieldErrors.get("record")}
      />
      <ValueExprInput
        label="版（rev・省略可）"
        value={p.expected_rev}
        onChange={(v) => patchParams(dispatch, node, { expected_rev: v === "" ? undefined : v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="空欄で実行時点の版"
        error={fieldErrors.get("expected_rev")}
      />
    </>
  );
}

export function DataRecordUpdateForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { tables } = useDataTables();
  const table = useTableOf(node, tables);
  const p = paramsOf<NodeParamsByType["data.record.update"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      <RecordAndRev {...props} />
      <ValueExprInput
        label="変更する値"
        value={p.patch as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { patch: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="object"
        placeholder='例: {"status": "承認"}（null で値を消す）'
        error={fieldErrors.get("patch")}
      />
      <FieldKeysHint table={table} />
    </div>
  );
}

export function DataRecordDeleteForm(props: FormProps) {
  const { tables } = useDataTables();
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      <RecordAndRev {...props} />
    </div>
  );
}

export function DataTransitionForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { tables } = useDataTables();
  const table = useTableOf(node, tables);
  const p = paramsOf<NodeParamsByType["data.transition"]>(node);
  const statusField = table?.schema.status_field
    ? table.schema.fields.find((f) => f.key === table.schema.status_field)
    : null;
  const options = statusField?.options ?? [];
  const literal = typeof p.to === "string" ? p.to : null;
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      <RecordAndRev {...props} />
      {options.length && (literal !== null || p.to === undefined) ? (
        <Field label="次の状態" error={fieldErrors.get("to")} hint="状態の遷移の規則（FSM）で許される先だけが実行できます">
          <Select value={literal ?? ""} onValueChange={(to) => patchParams(dispatch, node, { to })}>
            <SelectTrigger aria-label="次の状態" className="h-8 text-[13px]">
              <SelectValue placeholder="状態を選ぶ" />
            </SelectTrigger>
            <SelectContent>
              {options.map((o) => (
                <SelectItem key={o} value={o}>
                  {o}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      ) : (
        <ValueExprInput
          label="次の状態"
          value={p.to as ValueExpr | undefined}
          onChange={(v) => patchParams(dispatch, node, { to: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="例: 承認"
          error={fieldErrors.get("to")}
        />
      )}
    </div>
  );
}

export function DataBulkUpsertForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { tables } = useDataTables();
  const table = useTableOf(node, tables);
  const p = paramsOf<NodeParamsByType["data.bulk_upsert"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} tables={tables} />
      <ValueExprInput
        label="登録する行"
        value={p.rows as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { rows: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="array"
        placeholder='例: [{"data": {"name": "書籍"}}, {"id": "…", "rev": 3, "data": {"amount": 0}}]'
        error={fieldErrors.get("rows")}
      />
      <FieldKeysHint table={table} />
      <label className="flex items-center justify-between gap-3 text-xs">
        <span>
          <span className="font-medium">不合格の行があれば何も登録しない</span>
          <span className="block text-[11px] text-muted-foreground">オフなら合格した行だけを登録します</span>
        </span>
        <Switch
          aria-label="不合格の行があれば何も登録しない"
          checked={Boolean(p.abort_on_invalid)}
          onCheckedChange={(on) => patchParams(dispatch, node, { abort_on_invalid: on || undefined })}
        />
      </label>
    </div>
  );
}
