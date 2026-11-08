"use client";

/// 「CSV を集計」（csv.query）のフォーム（D-17・#1037）。
///
/// 集計するファイル（CSV か Excel）はテーブル `data`、つなぐ CSV（`tables`）はそれぞれに付けた名前のテーブルとして、
/// 読み取りだけの SQL で集計する。列の案内と試しに実行は、本人の権限で読む（ワークフローの実行や保存はしない）。

import * as React from "react";
import { Play, Plus, X } from "lucide-react";

import { DrivePicker } from "@/components/chat/drive-picker";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import { deriveTableName, isValidTableName, MAX_EXTRA_TABLES, PRIMARY_TABLE } from "@/lib/csv/table-name";
import type { NodeResponse } from "@/lib/storage";
import {
  getSchema,
  runQuery,
  TabularLoadError,
  type QueryTable,
  type SchemaResponse,
  type TableResponse,
} from "@/lib/tabular-api";
import { nodeErrorView } from "../validation";
import { BLANK_PICK_MESSAGE, BLANK_REQUIRED_MESSAGE, Field } from "./common";
import type { FormProps } from "./forms-basic";
import {
  FileField,
  FilePick,
  isCsvFile,
  isExcelFile,
  isQueryableFile,
  rememberDriveItem,
  useDriveItem,
  UUID_RE,
} from "./forms-files";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";
import { valueAtParamsPath } from "./value-refs";

/// 「CSV を集計」のファイルの選び方（CSV と Excel・D-17）。
const CSV_QUERY_PICKER = {
  accept: (n: NodeResponse) => isQueryableFile(n),
  pickerTitle: "CSV か Excel のファイルを選ぶ",
  pickerDescription: "集計するファイルを選んでください（CSV と Excel のファイルだけを表示しています）。",
};

/// 列の型（集計で読むときの DuckDB の型名）の呼び名（値の種類の呼び名にそろえる）。
function typeLabel(t: string): string {
  const u = t.toUpperCase();
  if (/^(TINYINT|SMALLINT|INTEGER|BIGINT|HUGEINT|UTINYINT|USMALLINT|UINTEGER|UBIGINT)$/.test(u)) return "整数";
  if (/INT|DOUBLE|FLOAT|DECIMAL|NUMERIC|REAL/.test(u)) return "数値";
  if (u.startsWith("TIMESTAMP")) return "日時";
  if (u === "DATE") return "日付";
  if (u.startsWith("TIME")) return "時刻";
  if (u.startsWith("BOOL")) return "はい／いいえ";
  return "テキスト";
}

/// 集計の結果の値の渡り方（rows はテキスト・items は種類のまま・GAP-40）。
const RESULT_VALUES_HINT =
  "結果の行（rows）は、値をすべてテキストで渡します。数値やはい／いいえを種類のまま使うときは、行の値（items）を選び、列の名前で引きます。";

/// CSV の列（本人の権限でスキーマを読む）とテーブル名の案内。読めないときはサーバーの理由（直し方つき）を出す。
/// `tableNames` はつなぐ CSV の名前（SQL で読む名前・固定値で書いたものだけ）。
function CsvColumnsHint({ fileId, tableNames }: { fileId: string | null; tableNames: string[] }) {
  const [state, setState] = React.useState<{ id: string; schema: SchemaResponse | null; reason: string | null } | null>(
    null,
  );
  React.useEffect(() => {
    if (!fileId) return;
    let active = true;
    getSchema(fileId)
      .then((schema) => active && setState({ id: fileId, schema, reason: null }))
      .catch(
        (e) =>
          active &&
          setState({
            id: fileId,
            schema: null,
            reason: e instanceof TabularLoadError && e.unreadable ? e.message : null,
          }),
      );
    return () => {
      active = false;
    };
  }, [fileId]);
  const current = state && state.id === fileId ? state : null;
  return (
    <div className="space-y-1 rounded-md bg-muted/50 p-2 text-[11px] leading-relaxed text-muted-foreground" data-testid="csv-columns">
      <p>
        テーブル名は <code className="font-mono text-foreground">data</code> です
        {tableNames.length > 0 ? (
          <>
            （つなぐ CSV は{" "}
            {tableNames.map((n, i) => (
              <React.Fragment key={`${n}-${i}`}>
                {i > 0 ? "、" : ""}
                <code className="font-mono text-foreground">{n}</code>
              </React.Fragment>
            ))}
            ）
          </>
        ) : null}
        。日本語や空白を含む列名は <code className="font-mono text-foreground">&quot;列名&quot;</code>{" "}
        のように二重引用符で囲みます。
      </p>
      {!fileId ? null : !current ? (
        <p>列を読み込み中…</p>
      ) : current.schema === null ? (
        <p className="text-destructive">
          {current.reason ?? "列を読み取れませんでした。CSV か Excel として読めないか、見る権限がありません。"}
        </p>
      ) : (
        <>
          <p>
            列:{" "}
            {current.schema.columns.map((c, i) => (
              <span key={`${c}-${i}`}>
                {i > 0 ? "、" : ""}
                <code className="font-mono text-foreground">{c}</code>
                {current.schema!.column_types[i] ? `（${typeLabel(current.schema!.column_types[i])}）` : ""}
              </span>
            ))}
            {typeof current.schema.total_rows === "number" ? `・${current.schema.total_rows} 行` : ""}
          </p>
          {typeof current.schema.header_row === "number" ? (
            <p data-testid="csv-header-row">
              {current.schema.header_row} 行目を列の名前にしています。それより上の行は表題とみなして読みません。
            </p>
          ) : null}
          <ExcelSheets sheets={current.schema.sheets} />
        </>
      )}
      <p data-testid="csv-result-values-hint">{RESULT_VALUES_HINT}</p>
    </div>
  );
}

/// SQL に書くシートの名前（二重引用符で囲み、中の二重引用符は重ねる）。
function sheetIdent(name: string): string {
  return `"${name.replaceAll('"', '""')}"`;
}

/// Excel のシートの一覧と、ほかのシートの読み方（GAP-84）。CSV は出さない。
function ExcelSheets({ sheets }: { sheets: SchemaResponse["sheets"] }) {
  if (sheets.length === 0) return null;
  const example = sheets.find((s) => !s.data && s.usable);
  return (
    <div data-testid="csv-sheets">
      <p>
        シート:{" "}
        {sheets.map((s, i) => (
          // シートごとにまとめて折り返す（呼び名の途中で行を変えない）。
          <span key={`${s.name}-${i}`} className="inline-block">
            <code className="font-mono text-foreground">{sheetIdent(s.name)}</code>
            {s.data ? "（data）" : !s.usable ? "（名前が重なるため、名前では読めません）" : s.hidden ? "（隠したシート）" : ""}
            {i < sheets.length - 1 ? "、" : ""}
          </span>
        ))}
      </p>
      {example ? (
        <p>
          ほかのシートは <code className="font-mono text-foreground">FROM {sheetIdent(example.name)}</code>{" "}
          のように、シートの名前で読めます。
        </p>
      ) : null}
    </div>
  );
}

const PREVIEW_ROWS = 5;

/// 試し実行（本人の権限で SQL を流し、上位数行だけを見せる・ワークフローの実行や保存はしない）。つなぐ CSV も
/// 同じ名前で渡す。つなぐ CSV を前の結果から指定しているとき・名前やファイルを直す必要があるときは試せない
/// （`blocked` がその理由）。
function CsvPreview({
  fileId,
  sql,
  tables,
  blocked,
}: {
  fileId: string | null;
  sql: string;
  tables: QueryTable[];
  blocked: string | null;
}) {
  const [running, setRunning] = React.useState(false);
  // 結果は流したときのファイルと SQL とつなぐ CSV に紐づける（どれかを変えたら古い結果を出さない）。
  const [result, setResult] = React.useState<{ key: string; table: TableResponse | null; error: string | null } | null>(
    null,
  );
  const key = `${fileId ?? ""}\n${sql}\n${JSON.stringify(tables)}`;
  const shown = result && result.key === key ? result : null;
  const run = async () => {
    if (!fileId || !sql.trim() || blocked) return;
    setRunning(true);
    try {
      const table = await runQuery(fileId, sql, tables);
      setResult({ key, table, error: null });
    } catch (e) {
      setResult({ key, table: null, error: e instanceof Error ? e.message : String(e) });
    } finally {
      setRunning(false);
    }
  };
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs"
          loading={running}
          disabled={!fileId || !sql.trim() || blocked !== null || running}
          onClick={() => void run()}
        >
          {!running ? <Play className="size-3.5" aria-hidden /> : null}
          試しに実行
        </Button>
        <span className="text-[11px] text-muted-foreground">
          {!fileId ? "ファイルを選ぶと試せます" : (blocked ?? "あなたの権限で流し、上位 5 行を表示します")}
        </span>
      </div>
      {shown?.error ? (
        <p role="alert" className="rounded-md border border-destructive/40 bg-destructive/5 px-2 py-1.5 text-[11px] text-destructive">
          {shown.error}
        </p>
      ) : null}
      {shown?.table ? <PreviewTable table={shown.table} /> : null}
    </div>
  );
}

function PreviewTable({ table }: { table: TableResponse }) {
  const rows = table.rows.slice(0, PREVIEW_ROWS);
  return (
    <div className="space-y-1" data-testid="csv-preview">
      <div className="max-w-full overflow-x-auto rounded-md border scrollbar-subtle">
        <table className="w-full border-collapse text-[11px]">
          <thead>
            <tr>
              {table.columns.map((c, i) => (
                <th key={`${c}-${i}`} className="whitespace-nowrap border-b bg-muted/50 px-2 py-1 text-left font-medium">
                  {c}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i} className="border-b last:border-b-0">
                {r.map((v, j) => (
                  <td key={j} className="max-w-48 truncate whitespace-nowrap px-2 py-1" title={v ?? ""}>
                    {v ?? ""}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p className="text-[11px] text-muted-foreground">
        {table.rows.length === 0
          ? "該当する行はありません"
          : `${table.rows.length} 行${table.truncated ? "（上限で打ち切り）" : ""}のうち上位 ${rows.length} 行`}
      </p>
    </div>
  );
}

export function CsvQueryForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const p = paramsOf<NodeParamsByType["csv.query"]>(node);
  const fileId = typeof p.file === "string" && UUID_RE.test(p.file) ? p.file : null;
  const sql = p.sql === undefined || typeof p.sql === "string" ? ((p.sql as string | undefined) ?? "") : null;
  const primary = useDriveItem(fileId).item;
  const tables = p.tables as ValueExpr | undefined;
  const items = Array.isArray(tables) ? tables.map(tableItem) : [];
  const preview = previewTables(tables);
  return (
    <div className="space-y-4">
      <FileField
        {...props}
        {...CSV_QUERY_PICKER}
        label="集計する CSV か Excel のファイル"
        hint="有効化のとき、このファイルの閲覧権限を渡します。Excel のファイルは、表示している左端のシートを data として読み、ほかのシートは名前で読めます。表の上にある表題の行は、列の名前にしません。"
      />
      <CsvTablesField {...props} primaryExcel={primary !== null && isExcelFile(primary)} />
      {sql !== null ? (
        <Field label="SQL（読み取りだけの SELECT）" error={fieldErrors.get("sql")}>
          <Textarea
            aria-label="SQL"
            value={sql}
            rows={5}
            spellCheck={false}
            autoCapitalize="none"
            autoCorrect="off"
            placeholder="SELECT * FROM data LIMIT 100"
            aria-invalid={fieldErrors.get("sql") ? true : undefined}
            onChange={(e) => patchParams(dispatch, node, { sql: e.target.value })}
            className="font-mono text-xs md:text-xs"
          />
        </Field>
      ) : (
        <ValueExprInput
          label="SQL（読み取りだけの SELECT）"
          text
          value={p.sql as ValueExpr}
          onChange={(v) => patchParams(dispatch, node, { sql: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          multiline
          error={fieldErrors.get("sql")}
        />
      )}
      <CsvColumnsHint
        fileId={fileId}
        tableNames={items.flatMap((t) => (typeof t.name === "string" && t.name !== "" ? [t.name] : []))}
      />
      {sql !== null ? (
        <CsvPreview
          fileId={fileId}
          sql={sql}
          tables={"tables" in preview ? preview.tables : []}
          blocked={"blocked" in preview ? preview.blocked : null}
        />
      ) : null}
    </div>
  );
}

// ── つなぐ CSV（tables）──────────────────────────────────────────────────

/// つなぐ CSV の 1 件（IR では `{ "name": …, "file": … }`・どちらも前の結果から指定できる）。
type TableItem = { name?: ValueExpr; file?: ValueExpr };

function tableItem(raw: unknown): TableItem {
  return raw !== null && typeof raw === "object" && !Array.isArray(raw) ? (raw as TableItem) : {};
}

/// 前の結果から指定した値（`$from`・`$template`）か。
function isExpr(v: unknown): boolean {
  return v !== null && typeof v === "object";
}

/// つなぐ CSV の選び方（CSV だけ・Excel はつなげない）。
const TABLE_PICKER = {
  accept: (n: NodeResponse) => isCsvFile(n),
  pickerTitle: "つなぐ CSV を選ぶ",
  pickerDescription: "集計するファイルとつなぐ CSV を選んでください（.csv のファイルだけを表示しています）。",
};

/// つなぐ CSV の名前の誤り（形・data との重なり・重複）。最後に確かめるのは実行の側で、ここは書いている間に気づける
/// ようにする。前の結果から指定した名前は確かめない。
function tableNameError(items: TableItem[], i: number): string | null {
  const name = items[i].name ?? "";
  if (typeof name !== "string") return null;
  if (name === "") return "SQL での名前を入れてください。";
  if (name === PRIMARY_TABLE) return `${PRIMARY_TABLE} は集計するファイルの名前です。ほかの名前にしてください。`;
  if (!isValidTableName(name)) return "英小文字・数字・_ で書き、先頭は数字以外にしてください。";
  if (items.findIndex((t) => t.name === name) !== i) return "同じ名前がほかにもあります。";
  return null;
}

/// 試しに実行で渡すつなぐ CSV。渡せないときは、試せない理由。
function previewTables(value: ValueExpr | undefined): { tables: QueryTable[] } | { blocked: string } {
  if (value === undefined) return { tables: [] };
  const dynamic = { blocked: "つなぐ CSV を前の結果から指定しているときは、試せません" };
  if (!Array.isArray(value)) return dynamic;
  const items = value.map(tableItem);
  const tables: QueryTable[] = [];
  for (const [i, t] of items.entries()) {
    if (isExpr(t.name) || isExpr(t.file)) return dynamic;
    if (typeof t.name !== "string" || typeof t.file !== "string" || !UUID_RE.test(t.file) || tableNameError(items, i)) {
      return { blocked: "つなぐ CSV の名前とファイルを直すと試せます" };
    }
    tables.push({ name: t.name, file_id: t.file });
  }
  return { tables };
}

/// つなぐ CSV のファイルが空か（項目が無い・空の文字。前の結果から指定した値は空と見ない・readiness と同じ）。
function fileBlank(t: TableItem): boolean {
  return t.file === undefined || t.file === null || (typeof t.file === "string" && t.file.trim() === "");
}

/// 行の key（並びの添字ではなく、行を足したときに振る印・IR には書かない）。前の行を外したとき、後の行の欄の状態
/// （ファイルの「ドライブから選ぶ／前の結果から指定する」の切り替えなど）が前の行から移らないようにする。並びが外から
/// 変わった（取り消し・読み直し）ときは、足りない分を振り足し、余る分を落とす。
function useRowKeys(count: number): { keys: readonly number[]; remove: (i: number) => void } {
  const ref = React.useRef<{ keys: number[]; next: number }>({ keys: [], next: 0 });
  const rows = ref.current;
  while (rows.keys.length < count) rows.keys.push(rows.next++);
  if (rows.keys.length > count) rows.keys.length = count;
  return { keys: rows.keys, remove: (i) => void rows.keys.splice(i, 1) };
}

/// つなぐ CSV（`tables`・#1037）。足した CSV は、付けた名前の表として SQL で読める（集計するファイルは data）。
///
/// 空のときは「つなぐ CSV を足す」のボタンだけを出す。ドライブから CSV を選ぶと 1 件足し、名前はファイル名から付ける
/// （英字の無い名前は t1 のような番号）。足した後は足した行の「SQL での名前」へフォーカスを移し、選ばずに閉じたら
/// 足すボタンへ戻す（DrivePicker の既定・BUG-261）。空のときも足した後も同じ根の要素の中に、足すボタンとピッカーを
/// 同じ位置で置く（作り直さない・戻す先のボタンを消さない）。
/// `tables` 全体を前の結果から指定しているときは、その値の欄を出す。
function CsvTablesField(props: FormProps & { primaryExcel: boolean }) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors, serverCheck, primaryExcel } = props;
  const value = paramsOf<NodeParamsByType["csv.query"]>(node).tables as ValueExpr | undefined;
  const [open, setOpen] = React.useState(false);
  const items = (Array.isArray(value) ? value : []).map(tableItem);
  const rowKeys = useRowKeys(items.length);
  const listRef = React.useRef<HTMLUListElement>(null);
  /// ピッカーで CSV を選んで閉じたか（閉じた後、足した行へフォーカスを移す）。
  const added = React.useRef(false);
  if (value !== undefined && !Array.isArray(value)) {
    return (
      <ValueExprInput
        label="つなぐ CSV"
        value={value}
        onChange={(v) => patchParams(dispatch, node, { tables: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        error={fieldErrors.get("tables")}
        hint='[{"name": "SQL での名前", "file": "CSV のファイル ID"}] の形のリストを渡します。'
        actions={
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-2 text-[11px]"
            onClick={() => patchParams(dispatch, node, { tables: undefined })}
          >
            外す
          </Button>
        }
      />
    );
  }
  const write = (next: TableItem[]) => patchParams(dispatch, node, { tables: next.length > 0 ? next : undefined });
  const update = (i: number, patch: TableItem) => write(items.map((t, j) => (j === i ? { ...t, ...patch } : t)));
  const remove = (i: number) => {
    rowKeys.remove(i);
    write(items.filter((_, j) => j !== i));
  };
  const full = items.length >= MAX_EXTRA_TABLES;
  // 欄そのものの誤り（形の誤りなど）。1 件ごとの誤りはその件の下に出すので、ここで同じ文を重ねない。空の欄の知らせ
  // （保存時の注意の名前・ファイルが空）も、行ごとに出すので重ねない。
  const listError = fieldErrors.get("tables");
  const fieldError =
    listError === BLANK_PICK_MESSAGE ||
    listError === BLANK_REQUIRED_MESSAGE ||
    items.some((_, i) => fieldErrors.has(`tables/${i}`))
      ? undefined
      : listError;
  // 行のファイルの欄の誤り。保存時の誤りのうち名前を指すもの（`/params/tables/{i}/name`）は名前の欄の下に出すので、
  // ファイルの欄へは渡さない（ファイルの下に名前の誤りを重ねない・GAP-126）。空なら「選んでください」（ほかの選ぶ欄と
  // 同じ・保存時の注意を待たない）。
  const nameAt = (i: number) => `/params/tables/${i}/name`;
  const rowCheck = (i: number) => ({ ...serverCheck, errors: serverCheck.errors.filter((e) => e.path !== nameAt(i)) });
  const rowErrors = (i: number, t: TableItem) => {
    const param = `tables/${i}`;
    const map = new Map(fieldErrors);
    map.delete(param);
    const fileError = nodeErrorView(rowCheck(i).errors).fields.get(param);
    if (fileError) map.set(param, fileError);
    else if (fileBlank(t)) map.set(param, BLANK_PICK_MESSAGE);
    return map;
  };
  // 名前の欄の保存時の誤り。手元の判定（`tableNameError`）が先で、手元で気づけない誤りだけを出す。確かめた時点の名前が
  // いまの名前と違えば出さない（直した後に古い誤りを残さない）。
  const serverNameError = (i: number, t: TableItem): string | null => {
    if (valueAtParamsPath(serverCheck.node?.params, nameAt(i)) !== t.name) return null;
    const at = serverCheck.errors.filter((e) => e.path === nameAt(i));
    return nodeErrorView(at).fields.get(`tables/${i}`) ?? null;
  };
  const example = items.find((t) => typeof t.name === "string" && isValidTableName(t.name))?.name as string | undefined;
  const picker = (
    <DrivePicker
      open={open}
      onOpenChange={setOpen}
      title={TABLE_PICKER.pickerTitle}
      description={TABLE_PICKER.pickerDescription}
      accept={TABLE_PICKER.accept}
      onSelect={(n) => {
        rememberDriveItem(n);
        const taken = items.flatMap((t) => (typeof t.name === "string" ? [t.name] : []));
        added.current = true;
        write([...items, { name: deriveTableName(n.name, taken), file: n.id }]);
      }}
      onCloseAutoFocus={(e) => {
        // 足したら足した行の「SQL での名前」へ移す。選ばずに閉じたら何もしない（DrivePicker の既定で、開いた
        // 「つなぐ CSV を足す」へ戻る）。
        const rows = added.current
          ? listRef.current?.querySelectorAll<HTMLElement>('[data-testid="csv-table"]')
          : undefined;
        added.current = false;
        const target = rows?.[rows.length - 1]?.querySelector<HTMLElement>("input");
        if (!target) return;
        e.preventDefault();
        target.focus();
      }}
    />
  );
  return (
    <div className="space-y-2" data-testid="csv-tables">
      {items.length > 0 ? (
        <fieldset className="min-w-0 space-y-2">
          <legend className="mb-0.5 text-xs font-medium text-foreground">つなぐ CSV</legend>
          <p className="text-[11px] text-muted-foreground">
            SQL では、集計するファイルを <code className="font-mono text-foreground">{PRIMARY_TABLE}</code>
            、つなぐ CSV を「SQL での名前」で読みます。有効化のとき、それぞれの閲覧権限を渡します。
          </p>
          <ul ref={listRef} className="space-y-2">
            {items.map((t, i) => {
              const nameError = tableNameError(items, i) ?? serverNameError(i, t);
              const nth = `${i + 1} つ目`;
              return (
                <li key={rowKeys.keys[i]} className="min-w-0 space-y-1.5 rounded-md border p-2" data-testid="csv-table">
                  <div className="flex min-w-0 items-center gap-1.5">
                    {isExpr(t.name) ? (
                      <div className="min-w-0 flex-1">
                        <ValueExprInput
                          label={`${nth}の CSV の SQL での名前`}
                          shownLabel="SQL での名前"
                          text
                          value={t.name}
                          onChange={(v) => update(i, { name: v })}
                          refCandidates={refCandidates}
                          inMapRegion={inMapRegion}
                        />
                      </div>
                    ) : (
                      <label className="flex min-w-0 flex-1 items-center gap-2">
                        <span className="shrink-0 text-[11px] text-muted-foreground">SQL での名前</span>
                        <Input
                          value={typeof t.name === "string" ? t.name : ""}
                          onChange={(e) => update(i, { name: e.target.value })}
                          aria-label={`${nth}の CSV の SQL での名前`}
                          aria-invalid={nameError ? true : undefined}
                          placeholder="例: master"
                          spellCheck={false}
                          autoCapitalize="none"
                          autoCorrect="off"
                          className="h-7 min-w-0 flex-1 font-mono text-xs md:text-xs"
                        />
                      </label>
                    )}
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-7 shrink-0"
                      aria-label={`${nth}の CSV を外す`}
                      onClick={() => remove(i)}
                    >
                      <X className="size-3.5" aria-hidden />
                    </Button>
                  </div>
                  {nameError ? <p className="text-[11px] text-destructive">{nameError}</p> : null}
                  <FilePick
                    {...TABLE_PICKER}
                    bare
                    value={t.file}
                    onChange={(v) => update(i, { file: v })}
                    refCandidates={refCandidates}
                    inMapRegion={inMapRegion}
                    fieldErrors={rowErrors(i, t)}
                    serverCheck={rowCheck(i)}
                    param={`tables/${i}`}
                    path={`tables/${i}/file`}
                    label={`${nth}のつなぐ CSV`}
                  />
                </li>
              );
            })}
          </ul>
          {primaryExcel ? (
            <p className="text-[11px] text-destructive">
              集計するファイルが Excel のときは、CSV をつなげません。集計するファイルに CSV を選んでください。
            </p>
          ) : null}
        </fieldset>
      ) : null}
      {fieldError ? <p className="text-[11px] text-destructive">{fieldError}</p> : null}
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs"
          disabled={full}
          onClick={() => setOpen(true)}
        >
          <Plus className="size-3.5" aria-hidden />
          つなぐ CSV を足す
        </Button>
        {full ? (
          <span className="text-[11px] text-muted-foreground">つなげる CSV は {MAX_EXTRA_TABLES} 個までです</span>
        ) : null}
      </div>
      {example ? (
        <p className="text-[11px] text-muted-foreground">
          例: <code className="font-mono text-foreground">SELECT * FROM data JOIN {example} ON …</code>
        </p>
      ) : null}
      {picker}
    </div>
  );
}
