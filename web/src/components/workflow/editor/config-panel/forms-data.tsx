"use client";

/// 業務データ（data.*）ノードのフォーム（Task 15.7 のノード・設定画面は Task 15.5）。
///
/// 対象テーブルは一覧から選ぶ（固定値の UUID）。固定値で選ぶと、有効化のときに渡す権限
/// （data_table の viewer / editor）の対象がそのテーブルに決まる。前の結果から渡す指定も残す。
/// 絞り込み・並べ替えはデータ画面と同じ部品で、保存形は data の `DataQuery`（IR をそのまま渡す）。

import * as React from "react";

import { isCreatedBy, isDerived } from "@/components/data/schema-field-form";
import { FilterControl, SortControl, type FilterExprValue, type FilterTexts } from "@/components/data/view-controls";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import type { DataQuery, DataTable } from "@/lib/data-api";
import { buildFilter, resolveColumns, splitFilter } from "@/lib/data-view";
import { cn } from "@/lib/utils";
import { Field, MoreInfo, NumberInput } from "./common";
import { DataTableSelect, isFsmTable, NoFsmTableNote, useDataTables, type DataTables } from "./data-tables";
import type { FormProps } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { RefPicker } from "./ref-picker";
import {
  InputFieldsContext,
  MapRegionContext,
  TriggerSourcesContext,
  ValueExprInput,
  type ComposeKeys,
} from "./value-expr-input";
import { isFromRef, type RefContext } from "./value-refs";

/// 対象テーブル（固定値の選択 ／ 前の結果から）。選んだテーブルを返す（列の候補に使う）。`eligible` は選べる
/// テーブル（状態を進めるは状態の進め方のあるテーブルだけ・BUG-105）。
function TableField({
  node,
  dispatch,
  refCandidates,
  inMapRegion,
  fieldErrors,
  source,
  eligible,
  ineligibleNote,
  children,
}: FormProps & {
  source: DataTables;
  eligible?: (t: DataTable) => boolean;
  ineligibleNote?: string;
  /// 一覧から選ぶときに、欄の下に足す注記。
  children?: React.ReactNode;
}) {
  const p = paramsOf<{ table: ValueExpr }>(node);
  const literal = typeof p.table === "string" ? p.table : null;
  const dynamic = p.table !== undefined && literal === null;
  const [mode, setMode] = React.useState<"pick" | "expr">(dynamic ? "expr" : "pick");
  return (
    <div className="space-y-1.5">
      {mode === "pick" ? (
        <Field label="対象のテーブル" error={fieldErrors.get("table")} hint="有効化のとき、このテーブルの権限を渡します">
          <DataTableSelect
            value={literal}
            source={source}
            eligible={eligible}
            ineligibleNote={ineligibleNote}
            // 選べないのは状態の進め方が無いテーブルだけ（状態を進める）なので、作る画面へ案内する（GAP-44）。
            stateFlowLink={eligible !== undefined}
            onChange={(id) => patchParams(dispatch, node, { table: id })}
          />
          {children}
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

/// ブロックの対象テーブル（固定値で束縛したもの）と、選ぶ欄に渡すテーブルの一覧（束縛済みのテーブルが一覧の
/// 200 件に入らなくても、1 件ずつ取って足す・BUG-68）。
function useNodeTable(node: FormProps["node"]): { source: DataTables; table: DataTable | null } {
  const p = paramsOf<{ table: ValueExpr }>(node);
  const literal = typeof p.table === "string" ? p.table : null;
  const source = useDataTables({ bound: [literal] });
  const found = source.lookup(literal);
  return { source, table: found.state === "ok" ? found.table : null };
}

/// 「項目を並べる」で選ぶ列（書ける列だけ・BUG-118）。状態の進め方のあるテーブルの状態の列も出さない（追加では
/// 最初の状態に決まり、変えるのは「状態を進める」だけ・data の record.rs）。名前は「{列名}（{キー}）」。
function columnKeys(table: DataTable | null): ComposeKeys | undefined {
  if (!table) return undefined;
  const status = table.schema.fsm_ref ? table.schema.status_field : null;
  return {
    options: table.schema.fields
      // 書けない列（計算の列とリンク先の値・付録 D の N3.3.2・追加した人が入る作成者の列）は、列の編集と同じ判定で外す。
      .filter((f) => !isDerived(f.type) && !isCreatedBy(f.type) && f.key !== status)
      .map((f) => ({
        key: f.key,
        label: f.display_name && f.display_name !== f.key ? `${f.display_name}（${f.key}）` : f.key,
        text: f.type === "text",
        numeric: f.type === "number",
      })),
  };
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

/// 「探す条件」の文言（データの画面の「絞り込み」と同じ部品をワークフローの言葉で出す・付録 D の N3.3.1）。
const QUERY_FILTER_TEXTS: FilterTexts = {
  title: "探す条件",
  subtitle: "すべての条件に合うレコード",
  advanced: "ここでは編集できない条件が入っています（チャットで作った条件など）。",
  clearAdvanced: "この条件を外す",
  empty: "条件はまだありません。",
  apply: "この条件で探す",
};

/// 「探す条件」の比べる値を、入力や結果から選ぶ（`$from` のまま保存する・BUG-98）。参照でない式（チャットで
/// 作った差し込みの文章など）は、ここでは中身を変えずにそのまま残す。
function useQueryExprValue(props: FormProps): FilterExprValue {
  const inputFields = React.useContext(InputFieldsContext);
  const triggerSources = React.useContext(TriggerSourcesContext);
  const mapLabel = React.useContext(MapRegionContext);
  const { refCandidates, inMapRegion } = props;
  const ctx = React.useMemo<RefContext>(
    () => ({ inputFields, ancestors: refCandidates, inMapRegion: Boolean(inMapRegion), mapLabel, triggerSources }),
    [inputFields, refCandidates, inMapRegion, mapLabel, triggerSources],
  );
  return React.useMemo(
    () => ({
      render: ({ label, value, onChange }) =>
        value !== null && !isFromRef(value) ? (
          <p className="rounded-md border border-dashed px-2.5 py-1.5 text-[11px] text-muted-foreground" data-testid="filter-expr-fixed">
            ここでは編集できない値です（チャットで作った値など）。
          </p>
        ) : (
          <RefPicker
            value={value !== null && isFromRef(value) ? value : null}
            onChange={(ref) => onChange(ref as Record<string, unknown>)}
            ctx={ctx}
            ariaLabel={label}
          />
        ),
    }),
    [ctx],
  );
}

export function DataQueryForm(props: FormProps) {
  const { node, dispatch, fieldErrors } = props;
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.query"]>(node);
  const query: DataQuery = (p.query as DataQuery | undefined) ?? {};
  const columns = React.useMemo(() => (table ? resolveColumns(table.schema.fields, null, new Set()) : []), [table]);
  // 比べる値が参照（$from）の条件も行として持つ（文字に直して参照を消さない・BUG-98）。
  const f = splitFilter(query.filter, { exprs: true });
  const exprValue = useQueryExprValue(props);
  // 読む件数の上限の誤り（保存時の照合・`/params/query/limit`・BUG-108）は、その欄の下に出す（探す条件の下に重ねない）。
  const limitError = fieldErrors.get("query/limit") ?? null;
  const queryError = fieldErrors.get("query") ?? null;
  const setQuery = (next: DataQuery) => {
    const cleaned = Object.fromEntries(Object.entries(next).filter(([, v]) => v !== undefined && v !== null));
    patchParams(dispatch, node, { query: Object.keys(cleaned).length ? cleaned : undefined });
  };
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      {table ? (
        <div className="space-y-1.5">
          <span className="text-xs font-medium">探す条件と並べ替え（省略可）</span>
          <div className="flex flex-wrap gap-1 rounded-md border p-1">
            <FilterControl
              columns={columns}
              rows={f.rows}
              advanced={f.advanced !== null}
              texts={QUERY_FILTER_TEXTS}
              exprValue={exprValue}
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
          {queryError && queryError !== limitError ? (
            <p className="text-[11px] text-destructive">{queryError}</p>
          ) : null}
        </div>
      ) : null}
      <Field
        label="読む件数の上限（省略可）"
        hint="空欄なら 50 件、最大 200 件です。条件に合うレコードがこれより多いときは、並べ替えの順で先頭から読みます。"
        error={limitError}
      >
        <NumberInput
          value={typeof query.limit === "number" ? query.limit : null}
          min={1}
          max={200}
          placeholder="例: 20"
          error={Boolean(limitError)}
          onChange={(n) => setQuery({ ...query, limit: n ?? undefined })}
        />
      </Field>
    </div>
  );
}

export function DataRecordCreateForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.record.create"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      <ValueExprInput
        label="レコードの値"
        value={p.data as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { data: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="object"
        keys={columnKeys(table)}
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
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.record.update"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      <RecordAndRev {...props} />
      <ValueExprInput
        label="変更する値"
        value={p.patch as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { patch: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="object"
        keys={columnKeys(table)}
        placeholder={patchExample(table)}
        error={fieldErrors.get("patch")}
      />
      <StatusFieldHint table={table} patch={p.patch} />
      <FieldKeysHint table={table} />
    </div>
  );
}

/// 「変更する値」の例（そのテーブルの直接書ける列で作る。FSM の状態の列は遷移でしか変わらないので例に
/// 使わない・C2 N7）。
function patchExample(table: DataTable | null): string {
  const f = table?.schema.fields.find(
    (x) => x.key !== table.schema.status_field && (x.type === "text" || x.type === "number" || x.type === "select"),
  );
  if (!f) return '例: {"memo": "確認済み"}（null で値を消す）';
  const v = f.type === "number" ? "1200" : f.type === "select" ? JSON.stringify(f.options?.[0] ?? "値") : '"確認済み"';
  return `例: {"${f.key}": ${v}}（null で値を消す）`;
}

/// FSM の状態の列は「データを更新」では変えられない（遷移の定義と実行できる人の条件を通すため）。
function StatusFieldHint({ table, patch }: { table: DataTable | null; patch: unknown }) {
  const key = table?.schema.fsm_ref ? table.schema.status_field : null;
  if (!table || !key) return null;
  const f = table.schema.fields.find((x) => x.key === key);
  const touches = Boolean(patch && typeof patch === "object" && !Array.isArray(patch) && key in (patch as Record<string, unknown>));
  return (
    <p
      className={cn("text-[11px] leading-relaxed", touches ? "text-destructive" : "text-muted-foreground")}
      role={touches ? "alert" : undefined}
      data-testid="workflow-status-field-hint"
    >
      「{f?.display_name || key}」（<code className="font-mono">{key}</code>）は状態の列なので、ここでは変えられません。状態を変えるときは「状態を進める」のブロックを使ってください。
    </p>
  );
}

export function DataRecordDeleteForm(props: FormProps) {
  const { source } = useNodeTable(props.node);
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      <RecordAndRev {...props} />
    </div>
  );
}

export function DataTransitionForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.transition"]>(node);
  const statusField = table?.schema.status_field
    ? table.schema.fields.find((f) => f.key === table.schema.status_field)
    : null;
  const options = statusField?.options ?? [];
  const literal = typeof p.to === "string" ? p.to : null;
  return (
    <div className="space-y-4">
      {/* 状態の進め方の無いテーブルは選べない（承認を待つと同じ・選べても実行のたびに失敗する・BUG-105）。 */}
      <TableField
        {...props}
        source={source}
        eligible={isFsmTable}
        ineligibleNote="このテーブルには状態の進め方がないため、状態を進められません。"
      >
        <NoFsmTableNote source={source} testId="transition-no-fsm-table" />
      </TableField>
      <RecordAndRev {...props} />
      {options.length && (literal !== null || p.to === undefined) ? (
        <Field
          label="次の状態"
          error={fieldErrors.get("to")}
          // 進められるかは、状態の進め方の決まりに加えて、進める人の条件でも決まる（付録 D の N3.3.5）。
          hint="レコードのいまの状態から、この状態へ進めます。進められるかは、テーブルの状態の進め方と、実行する人（自動実行ではオンにした人）で決まります。"
        >
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

/// 「データをまとめて登録」の、形の合わないレコードがあれば止めるスイッチ（付録 D の N3.3.6）。
const ABORT_ON_INVALID_LABEL = "1 件でも合わなければ、どれも登録しない";

export function DataBulkUpsertForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.bulk_upsert"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
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
      <div className="space-y-1">
        <label className="flex items-center justify-between gap-3 text-xs">
          <span>
            <span className="font-medium">{ABORT_ON_INVALID_LABEL}</span>
            {/* 「何も登録しない」は言い過ぎだった（形の検査の後に、ほかの書き込みと重なって弾かれたレコードが
                あっても、残りは登録する・どちらでもブロックは失敗しない・data の bulk・BUG-104）。 */}
            <span className="block text-[11px] text-muted-foreground" data-testid="bulk-abort-note">
              オフなら、形の合うレコードだけを登録します。どちらでもブロックは失敗にならないので、結果の件数で確かめてください。
            </span>
          </span>
          <Switch
            aria-label={ABORT_ON_INVALID_LABEL}
            checked={Boolean(p.abort_on_invalid)}
            onCheckedChange={(on) => patchParams(dispatch, node, { abort_on_invalid: on || undefined })}
          />
        </label>
        <MoreInfo testId="bulk-abort-more">
          オンでも、止めるのは形の合わないレコードがあるときだけです。ほかの書き込みと重なって登録できなかったレコード（同じ値を入れられない列など）があっても、残りは登録します。すべて登録できたかは、結果の「登録した件数」（rows_applied）と「渡した件数」（rows_total）が同じかで分かります。
        </MoreInfo>
      </div>
    </div>
  );
}
