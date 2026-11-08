"use client";

/// 業務データ（data.*）ノードのフォーム（Task 15.7 のノード・設定画面は Task 15.5・キーで追加か更新と足す数は GAP-16）。
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
import {
  buildFilter,
  isFilterable,
  isWorkflowQueryColumn,
  resolveColumns,
  splitFilter,
  type ColumnModel,
} from "@/lib/data-view";
import { cn } from "@/lib/utils";
import { nodeErrorView } from "../validation";
import {
  BLANK_PICK_MESSAGE,
  BLANK_REQUIRED_MESSAGE,
  Field,
  FieldInput,
  FieldSelectTrigger,
  MoreInfo,
  NumberInput,
} from "./common";
import { DataTableSelect, isFsmTable, NoFsmTableNote, useDataTables, type DataTables } from "./data-tables";
import type { FormProps, ServerCheck } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { RefPicker } from "./ref-picker";
import {
  InputFieldsContext,
  MapRegionContext,
  TriggerSourcesContext,
  ValueExprInput,
  type ComposeKeys,
} from "./value-expr-input";
import { isFromRef, valueAtParamsPath, type RefContext } from "./value-refs";

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
/// 200 件に入らなくても、1 件ずつ取って足す・BUG-68）。`unknown` は、テーブルが決まっているのに列を読めないとき
/// （前の結果から指定する・見えない・読み込みの失敗）。列から選ぶ欄は、代わりにキーを打つ欄にする。
function useNodeTable(node: FormProps["node"]): { source: DataTables; table: DataTable | null; unknown: boolean } {
  const p = paramsOf<{ table: ValueExpr }>(node);
  const literal = typeof p.table === "string" ? p.table : null;
  const source = useDataTables({ bound: [literal] });
  const found = source.lookup(literal);
  const dynamic = p.table !== undefined && p.table !== null && literal === null;
  return {
    source,
    table: found.state === "ok" ? found.table : null,
    unknown: dynamic || found.state === "unreadable" || found.state === "failed",
  };
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

/// 「足す数」で選ぶ列（数の列のうち書ける列だけ・GAP-16）。
function numberKeys(table: DataTable | null): ComposeKeys | undefined {
  const keys = columnKeys(table);
  if (!keys || !table) return keys;
  const numeric = new Set(table.schema.fields.filter((f) => f.type === "number").map((f) => f.key));
  return { options: keys.options.filter((o) => numeric.has(o.key)) };
}

/// 値の欄が空か（項目なし・空の文字・中身の無いまとまり）。
function isEmptyValue(v: unknown): boolean {
  if (v === undefined || v === null) return true;
  if (typeof v === "string") return v.trim() === "";
  return typeof v === "object" && !Array.isArray(v) && Object.keys(v as object).length === 0;
}

/// 「同じレコードを探す列（省略可）」の「選ばない」の値（選択の部品は空の文字を値にできない）。
const PICK_NONE = "__none__";

/// 同じレコードを探す列（重複を許さない列だけ・キーで追加か更新とまとめて登録・GAP-16）。保存時にもサーバーが同じ
/// 決まりで照らす（重複を許す列は ir.match_not_unique）。テーブルの列を読めないとき（`unknown`・前の結果から指定する
/// テーブルなど）は、選ぶ代わりに列のキーを打つ（`typedHint` を添える）。
function MatchOnField({
  node,
  dispatch,
  fieldErrors,
  table,
  unknown,
  optional,
  hint,
  typedHint,
}: FormProps & { table: DataTable | null; unknown: boolean; optional?: boolean; hint: string; typedHint: string }) {
  const p = paramsOf<{ match_on?: string }>(node);
  const value = (p.match_on ?? "").trim();
  const label = optional ? "同じレコードを探す列（省略可）" : "同じレコードを探す列";
  if (unknown) {
    // 空の欄の促しは、選ぶ欄の「選んでください」ではなく打つ欄の「入力してください」にする。
    const error = fieldErrors.get("match_on");
    return (
      <Field label={label} hint={typedHint} error={error === BLANK_PICK_MESSAGE ? BLANK_REQUIRED_MESSAGE : error}>
        <FieldInput
          value={p.match_on ?? ""}
          onChange={(e) => {
            const v = e.target.value;
            patchParams(dispatch, node, { match_on: v.trim() === "" && optional ? undefined : v });
          }}
          placeholder="例: sku"
          className="h-8 font-mono text-[13px]"
          data-testid="match-on-input"
        />
      </Field>
    );
  }
  const status = table?.schema.fsm_ref ? table.schema.status_field : null;
  const options = table
    ? table.schema.fields.filter((f) => f.unique && !isDerived(f.type) && !isCreatedBy(f.type) && f.key !== status)
    : [];
  const name = (f: { key: string; display_name?: string | null }) =>
    f.display_name && f.display_name !== f.key ? `${f.display_name}（${f.key}）` : f.key;
  const known = options.some((f) => f.key === value);
  return (
    <Field label={label} hint={hint} error={fieldErrors.get("match_on")}>
      {table && options.length === 0 && !value ? (
        <p className="rounded-md border border-dashed px-2.5 py-1.5 text-[11px] leading-relaxed text-muted-foreground" data-testid="match-on-none">
          このテーブルには、重複を許さない列がありません。テーブルの列の設定で、探すのに使う列（品番など）の「重複を許さない」をオンにしてください。
        </p>
      ) : (
        <Select
          value={value || (optional ? PICK_NONE : "")}
          onValueChange={(v) =>
            patchParams(dispatch, node, { match_on: v === PICK_NONE ? (optional ? undefined : "") : v })
          }
        >
          <FieldSelectTrigger aria-label={label} className="h-8 text-[13px]" data-testid="match-on-select">
            <SelectValue placeholder="列を選ぶ" />
          </FieldSelectTrigger>
          <SelectContent>
            {optional ? <SelectItem value={PICK_NONE}>選ばない（ID の無いレコードはすべて追加）</SelectItem> : null}
            {options.map((f) => (
              <SelectItem key={f.key} value={f.key}>
                {name(f)}
              </SelectItem>
            ))}
            {value && !known ? <SelectItem value={value}>{value}</SelectItem> : null}
          </SelectContent>
        </Select>
      )}
    </Field>
  );
}

/// 「足す数」の欄（数の列ごとに足す数・負の数で減らす・GAP-16）。`creates` はキーで追加か更新（追加するときは 0 に足す）。
function AddField(props: FormProps & { table: DataTable | null; creates?: boolean; onAdd: (v: ValueExpr | undefined) => void }) {
  const { node, refCandidates, inMapRegion, fieldErrors, table, creates, onAdd } = props;
  const p = paramsOf<{ add?: ValueExpr }>(node);
  return (
    <div className="space-y-1">
      <ValueExprInput
        label="足す数（省略可）"
        value={p.add}
        onChange={(v) => onAdd(isEmptyValue(v) ? undefined : v)}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        json="object"
        keys={numberKeys(table)}
        numericNote={null}
        hint={
          creates
            ? "数の列に、この数を足して書きます。減らすときは負の数（-1 など）を入れます。追加するときは 0 に足します。"
            : "数の列に、この数を足して書きます。減らすときは負の数（-1 など）を入れます。"
        }
        placeholder='例: {"stock": -1}'
        error={fieldErrors.get("add")}
      />
      <MoreInfo testId="add-more">
        ほかの実行と同時に書き込んでも、足した数は失われません。足した結果が列の最小値・最大値を外れると、書き込まずに失敗します。在庫を 0 未満にしないときは、列の最小値を 0 にしておきます。
      </MoreInfo>
    </div>
  );
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

/// 保存時の照合が断った探す条件・並べ替えの列（BUG-244）。`filter` と `sort` は列の参照名 → 文言（その列の行の下に
/// 出す）、`all` は欄の下に出す文言（読む件数の上限の誤りは除く・重ねない）。どの列を指すかは、誤りの path の位置の
/// 値を検証した時点の IR から引く（検証の後に条件を直しても、今の並びに当てて取り違えない）。
function queryErrors(check: ServerCheck): { filter: Map<string, string>; sort: Map<string, string>; all: string[] } {
  const filter = new Map<string, string>();
  const sort = new Map<string, string>();
  const all: string[] = [];
  for (const e of check.errors) {
    const view = nodeErrorView([e]).fields;
    const message = view.get("query");
    if (!message || view.get("query/limit") === message) continue;
    if (!all.includes(message)) all.push(message);
    const path = e.path ?? "";
    const key = valueAtParamsPath(check.node?.params, path);
    if (typeof key !== "string") continue;
    if (/^\/params\/query\/filter(\/.*)?\/cmp\/field$/.test(path) && !filter.has(key)) filter.set(key, message);
    if (/^\/params\/query\/sort\/\d+\/field$/.test(path) && !sort.has(key)) sort.set(key, message);
  }
  return { filter, sort, all };
}

/// 探す条件と並べ替えで選べない列の説明（BUG-244）。使える列は索引のある列と重複を許さない列（とシステムの列）だけなので、
/// 隠した列があるときに言う。利用者の列が 1 つも使えないときは、使える列の名前を挙げる。
function hiddenColumnsNote(columns: ColumnModel[]): string | null {
  const hidden = columns.some((c) => isFilterable(c) && !isWorkflowQueryColumn(c));
  if (!hidden) return null;
  const how = "ほかの列を使うには、テーブルの「列の編集」でその列の索引をオンにしてください。";
  const userColumns = columns.filter((c) => !c.system && isWorkflowQueryColumn(c) && isFilterable(c));
  if (userColumns.length === 0) {
    const names = columns
      .filter((c) => isWorkflowQueryColumn(c) && isFilterable(c))
      .map((c) => `「${c.label}」`)
      .join("");
    return `このテーブルの列には索引が無いので、絞り込み・並べ替えに使えるのは${names}だけです。${how}`;
  }
  return `絞り込み・並べ替えに使えるのは、索引のある列と重複を許さない列だけです。${how}`;
}

export function DataQueryForm(props: FormProps) {
  const { node, dispatch, fieldErrors, serverCheck } = props;
  const { source, table } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.query"]>(node);
  const query: DataQuery = (p.query as DataQuery | undefined) ?? {};
  const columns = React.useMemo(() => (table ? resolveColumns(table.schema.fields, null, new Set()) : []), [table]);
  // 比べる値が参照（$from）の条件も行として持つ（文字に直して参照を消さない・BUG-98）。
  const f = splitFilter(query.filter, { exprs: true });
  const exprValue = useQueryExprValue(props);
  // 読む件数の上限の誤り（保存時の照合・`/params/query/limit`・BUG-108）は、その欄の下に出す（探す条件の下に重ねない）。
  const limitError = fieldErrors.get("query/limit") ?? null;
  // 探す条件と並べ替えの誤りは、すべて欄の下に出し、列を断った誤りはその列の行の下にも出す（BUG-244）。
  const errors = React.useMemo(() => queryErrors(serverCheck), [serverCheck]);
  const note = hiddenColumnsNote(columns);
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
            {/* ワークフローの経路では索引を作らないので、索引のある列と重複を許さない列だけを選ばせる（BUG-244）。 */}
            <FilterControl
              columns={columns}
              rows={f.rows}
              advanced={f.advanced !== null}
              texts={QUERY_FILTER_TEXTS}
              exprValue={exprValue}
              usable={isWorkflowQueryColumn}
              columnError={(key) => errors.filter.get(key) ?? null}
              onClearAdvanced={() => setQuery({ ...query, filter: buildFilter(f.rows, null, columns) ?? undefined })}
              onChange={(rows) => setQuery({ ...query, filter: buildFilter(rows, f.advanced, columns) ?? undefined })}
            />
            <SortControl
              columns={columns}
              sort={query.sort ?? []}
              maxKeys={4}
              usable={isWorkflowQueryColumn}
              columnError={(key) => errors.sort.get(key) ?? null}
              onChange={(sort) => setQuery({ ...query, sort: sort.length ? sort : undefined })}
            />
          </div>
          {note ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground" data-testid="query-columns-note">
              {note}
            </p>
          ) : null}
          {errors.all.map((message) => (
            <p key={message} className="text-[11px] text-destructive">
              {message}
            </p>
          ))}
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
      {/* レコードの ID は画面に出ないので、前の結果やきっかけから選ぶ（置いたばかりの空の欄は候補を開いて置く・D-83）。 */}
      <ValueExprInput
        label="対象のレコード"
        value={p.record}
        onChange={(v) => patchParams(dispatch, node, { record: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="レコード ID"
        error={fieldErrors.get("record")}
        preferFrom
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
      {/* 足す数に列があれば、変更する値は空でよい（サーバーの readiness と index.tsx の optionalBlank）。 */}
      <AddField {...props} table={table} onAdd={(add) => patchParams(dispatch, node, { add })} />
      <FieldKeysHint table={table} />
    </div>
  );
}

/// 「レコードを追加か更新」（GAP-16）: 同じレコードを探す列の値が同じレコードがあれば書き換え、なければ追加する。
export function DataRecordUpsertForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const { source, table, unknown } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.record.upsert"]>(node);
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      <MatchOnField
        {...props}
        table={table}
        unknown={unknown}
        hint="この列の値が同じレコードがあれば書き換え、なければ追加します。選べるのは、重複を許さない列だけです。"
        typedHint="重複を許さない列のキーを入れます。この列の値が同じレコードがあれば書き換え、なければ追加します。"
      />
      <ValueExprInput
        label="入れる値"
        value={p.data as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { data: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="object"
        keys={columnKeys(table)}
        hint="「同じレコードを探す列」の値も入れます。書き換えるときも追加するときも、この値を書きます。"
        placeholder='例: {"sku": "A-100", "name": "六角ボルト"}'
        error={fieldErrors.get("data")}
      />
      <AddField
        {...props}
        table={table}
        creates
        onAdd={(add) => patchParams(dispatch, node, { add })}
      />
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
  const { source, table, unknown } = useNodeTable(node);
  const p = paramsOf<NodeParamsByType["data.bulk_upsert"]>(node);
  const matched = Boolean(p.match_on?.trim());
  return (
    <div className="space-y-4">
      <TableField {...props} source={source} />
      <MatchOnField
        {...props}
        table={table}
        unknown={unknown}
        optional
        hint="選ぶと、ID の無いレコードはこの列の値で探し、あれば更新、なければ追加します。同じ値のレコードが 2 件にならないので、取り込み直しても増えません。"
        typedHint="重複を許さない列のキーを入れると、ID の無いレコードはこの列の値で探し、あれば更新、なければ追加します。"
      />
      <ValueExprInput
        label="登録する行"
        hint={
          matched
            ? "1 件ずつ {\"data\": {\"列のキー\": 値}} の形にし、「同じレコードを探す列」の値を入れます。同じ値の行が 2 つあると、どれも登録しません。"
            : undefined
        }
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
