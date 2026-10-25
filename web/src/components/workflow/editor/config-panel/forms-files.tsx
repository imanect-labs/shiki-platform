"use client";

/// ファイル（storage.* / csv.*）ノードのフォーム（BUG-5）。
///
/// 対象のファイル・フォルダはドライブから選ぶ（固定値の UUID）。固定値で選ぶと、有効化のときに渡す権限
/// （ファイルの viewer / editor・フォルダの editor）の対象がそれに決まる。前の結果から渡す指定も残す。
/// 選んだものの名前は ID から引き直して出す（開き直しても UUID だけにならない）。

import * as React from "react";
import { File as FileIcon, Play, X } from "lucide-react";

import { FolderPicker } from "@/components/artifacts/folder-picker";
import { DrivePicker } from "@/components/chat/drive-picker";
import { NodeIcon } from "@/components/drive/primitives";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { ValueExpr } from "@/generated/workflow-ir";
import { getNode, type NodeResponse } from "@/lib/storage";
import { getSchema, runQuery, type SchemaResponse, type TableResponse } from "@/lib/tabular-api";
import { cn } from "@/lib/utils";
import { Field, NumberInput } from "./common";
import type { FormProps } from "./forms-basic";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput } from "./value-expr-input";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// 名前の表示に要る分だけ（ピッカーの応答・`GET /files/{id}` のどちらからでも作れる）。
type DriveItem = Pick<NodeResponse, "id" | "name" | "kind"> & Partial<Pick<NodeResponse, "content_type">>;

/// ID → ドライブの項目（名前の表示用・ページを開いている間だけ持つ）。選んだときの応答もここへ入れる。
const driveItems = new Map<string, DriveItem>();

/// ID のファイル・フォルダを引く（名前の表示用）。見つからない・読めないときは `missing`。
export function useDriveItem(id: string | null): { item: DriveItem | null; missing: boolean; loading: boolean } {
  const [loaded, setLoaded] = React.useState<{ id: string; item: DriveItem | null } | null>(null);
  const valid = id !== null && UUID_RE.test(id);
  React.useEffect(() => {
    if (!valid || !id || driveItems.has(id)) return;
    let active = true;
    getNode(id)
      .then((n) => {
        driveItems.set(id, n);
        if (active) setLoaded({ id, item: n });
      })
      .catch(() => active && setLoaded({ id, item: null }));
    return () => {
      active = false;
    };
  }, [id, valid]);
  if (!id) return { item: null, missing: false, loading: false };
  if (!valid) return { item: null, missing: true, loading: false };
  const cached = driveItems.get(id);
  if (cached) return { item: cached, missing: false, loading: false };
  if (loaded?.id === id) return { item: loaded.item, missing: loaded.item === null, loading: false };
  return { item: null, missing: false, loading: true };
}

/// ID の名前（有効化の画面など、表示だけに使う）。引けないときは `fallback`。
export function DriveItemName({ id, fallback }: { id: string; fallback: string }) {
  const { item } = useDriveItem(id);
  return <>{item?.name ?? fallback}</>;
}

export function isCsvFile(n: Pick<NodeResponse, "name" | "content_type">): boolean {
  return n.name.toLowerCase().endsWith(".csv") || (n.content_type ?? "").startsWith("text/csv");
}

/// 「一覧から選ぶ ／ 前の結果から指定する」の切り替え（forms-data の TableField と同じ見た目）。
function ModeToggle({ expr, onToggle, pickLabel }: { expr: boolean; onToggle: () => void; pickLabel: string }) {
  return (
    <button
      type="button"
      className="text-[11px] text-primary underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      onClick={onToggle}
    >
      {expr ? pickLabel : "前の結果から指定する"}
    </button>
  );
}

/// 選んだもの（アイコン・名前）と「選ぶ」ボタンの行。
function PickedRow({
  label,
  kind,
  item,
  text,
  muted,
  danger,
  verb,
  onPick,
  onClear,
}: {
  label: string;
  kind: "file" | "folder";
  item: DriveItem | null;
  text: string;
  muted?: boolean;
  danger?: boolean;
  verb: string;
  onPick: () => void;
  onClear?: () => void;
}) {
  // 読み上げ・操作の名前（「保存先フォルダ（省略可）」→「保存先フォルダを選ぶ」）。
  const name = label.replace(/（[^）]*）$/, "");
  return (
    <div className={cn("flex min-w-0 items-center gap-2 rounded-md border px-2 py-1.5", danger && "border-destructive")}>
      {item ? (
        <NodeIcon kind={item.kind} name={item.name} contentType={item.content_type} className="size-5 shrink-0" />
      ) : kind === "folder" ? (
        <NodeIcon kind="folder" className="size-5 shrink-0 opacity-60" />
      ) : (
        <FileIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
      )}
      <span
        className={cn(
          "min-w-0 flex-1 truncate text-[13px]",
          muted && "text-muted-foreground",
          danger && "text-destructive",
        )}
        title={text}
      >
        {text}
      </span>
      {onClear ? (
        <Button
          variant="ghost"
          size="icon"
          className="size-7 shrink-0"
          aria-label={`${name}の指定を外す`}
          onClick={onClear}
        >
          <X className="size-3.5" aria-hidden />
        </Button>
      ) : null}
      <Button
        variant="outline"
        size="sm"
        className="h-7 shrink-0 px-2.5 text-xs"
        aria-label={`${name}を${verb}`}
        onClick={onPick}
      >
        {verb}
      </Button>
    </div>
  );
}

/// 前の結果から指定している値を「選ぶ」の表示に切り替えたとき（選ぶと置き換わる）。
const DYNAMIC_TEXT = "前の結果から指定しています（選ぶと置き換えます）";

type PickFieldProps = FormProps & {
  /// params の項目名（既定 `file` / `folder`）。
  param?: string;
  label: string;
  hint?: string;
};

/// 対象のファイル（ドライブから選ぶ ／ 前の結果から）。
export function FileField({
  node,
  dispatch,
  refCandidates,
  inMapRegion,
  fieldErrors,
  param = "file",
  label,
  hint,
  accept,
  pickerTitle = "ファイルを選ぶ",
  pickerDescription = "使うファイルを選んでください。",
}: PickFieldProps & {
  accept?: (n: NodeResponse) => boolean;
  pickerTitle?: string;
  pickerDescription?: string;
}) {
  const value = paramsOf<Record<string, ValueExpr>>(node)[param];
  const literal = typeof value === "string" && value !== "" ? value : null;
  const dynamic = value !== undefined && typeof value !== "string";
  const [mode, setMode] = React.useState<"pick" | "expr">(dynamic ? "expr" : "pick");
  const [open, setOpen] = React.useState(false);
  const { item, missing, loading } = useDriveItem(literal);
  const error = fieldErrors.get(param);
  const text = dynamic
    ? DYNAMIC_TEXT
    : loading
      ? "読み込み中…"
      : missing
        ? "ファイルが見つかりません（削除されたか、閲覧の権限がありません）"
        : (item?.name ?? "ファイルが選ばれていません");
  return (
    <div className="space-y-1.5">
      {mode === "pick" ? (
        <Field label={label} error={error} hint={hint}>
          <PickedRow
            label={label}
            kind="file"
            item={item}
            text={text}
            muted={!item}
            danger={missing}
            verb={literal ? "変更" : "選ぶ"}
            onPick={() => setOpen(true)}
          />
        </Field>
      ) : (
        <ValueExprInput
          label={label}
          value={value}
          onChange={(v) => patchParams(dispatch, node, { [param]: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="ファイル ID"
          error={error}
        />
      )}
      <ModeToggle expr={mode === "expr"} pickLabel="ドライブから選ぶ" onToggle={() => setMode(mode === "pick" ? "expr" : "pick")} />
      <DrivePicker
        open={open}
        onOpenChange={setOpen}
        title={pickerTitle}
        description={pickerDescription}
        accept={accept}
        onSelect={(n) => {
          driveItems.set(n.id, n);
          patchParams(dispatch, node, { [param]: n.id });
        }}
      />
    </div>
  );
}

/// 対象のフォルダ（ドライブから選ぶ ／ 前の結果から）。`optional` なら外せる（外すとマイドライブ）。
export function FolderField({
  node,
  dispatch,
  refCandidates,
  inMapRegion,
  fieldErrors,
  param = "folder",
  label,
  hint,
  optional,
}: PickFieldProps & { optional?: boolean }) {
  const value = paramsOf<Record<string, ValueExpr>>(node)[param];
  const literal = typeof value === "string" && value !== "" ? value : null;
  const dynamic = value !== undefined && typeof value !== "string";
  const [mode, setMode] = React.useState<"pick" | "expr">(dynamic ? "expr" : "pick");
  const [open, setOpen] = React.useState(false);
  const { item, missing, loading } = useDriveItem(literal);
  const error = fieldErrors.get(param);
  const text = dynamic
    ? DYNAMIC_TEXT
    : loading
      ? "読み込み中…"
      : missing
        ? "フォルダが見つかりません（削除されたか、権限がありません）"
        : (item?.name ?? (optional ? "マイドライブ（指定なし）" : "フォルダが選ばれていません"));
  return (
    <div className="space-y-1.5">
      {mode === "pick" ? (
        <Field label={label} error={error} hint={hint}>
          <PickedRow
            label={label}
            kind="folder"
            item={item}
            text={text}
            muted={!item}
            danger={missing}
            verb={literal ? "変更" : "選ぶ"}
            onPick={() => setOpen(true)}
            onClear={optional && value !== undefined ? () => patchParams(dispatch, node, { [param]: undefined }) : undefined}
          />
        </Field>
      ) : (
        <ValueExprInput
          label={label}
          value={value}
          onChange={(v) => patchParams(dispatch, node, { [param]: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          placeholder="フォルダ ID"
          error={error}
        />
      )}
      <ModeToggle expr={mode === "expr"} pickLabel="ドライブから選ぶ" onToggle={() => setMode(mode === "pick" ? "expr" : "pick")} />
      <FolderPicker
        open={open}
        onOpenChange={setOpen}
        onSelect={(choice) => {
          driveItems.set(choice.id, { id: choice.id, name: choice.name, kind: "folder" });
          patchParams(dispatch, node, { [param]: choice.id });
        }}
      />
    </div>
  );
}

// ── ファイル（storage.*）────────────────────────────────────────────────

export function StorageReadForm(props: FormProps) {
  return <FileField {...props} label="読むファイル" hint="有効化のとき、このファイルの閲覧権限を渡します" />;
}

/// 保存するファイルの種類（Content-Type）。省略すると application/octet-stream で保存される。
const CONTENT_TYPES: [string, string][] = [
  ["text/plain", "テキスト（.txt）"],
  ["text/markdown", "Markdown（.md）"],
  ["text/csv", "CSV（.csv）"],
  ["application/json", "JSON（.json）"],
  ["text/html", "HTML（.html）"],
];
const NO_TYPE = "__none__";

function ContentTypeField({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["storage.write"]>(node);
  const value = p.content_type as ValueExpr | undefined;
  if (value !== undefined && typeof value !== "string") {
    return (
      <ValueExprInput
        label="ファイルの種類（省略可）"
        text
        value={value}
        onChange={(v) => patchParams(dispatch, node, { content_type: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: text/markdown"
        error={fieldErrors.get("content_type")}
      />
    );
  }
  const current = value || NO_TYPE;
  const known = CONTENT_TYPES.some(([t]) => t === current);
  return (
    <Field
      label="ファイルの種類（省略可）"
      hint="指定しないと種類の分からないファイル（application/octet-stream）として保存します"
      error={fieldErrors.get("content_type")}
    >
      <Select
        value={current}
        onValueChange={(v) => patchParams(dispatch, node, { content_type: v === NO_TYPE ? undefined : v })}
      >
        <SelectTrigger aria-label="ファイルの種類" className="h-8 text-[13px]">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={NO_TYPE}>指定しない</SelectItem>
          {CONTENT_TYPES.map(([t, label]) => (
            <SelectItem key={t} value={t}>
              {label}
            </SelectItem>
          ))}
          {!known && current !== NO_TYPE ? <SelectItem value={current}>{current}</SelectItem> : null}
        </SelectContent>
      </Select>
    </Field>
  );
}

export function StorageWriteForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const p = paramsOf<NodeParamsByType["storage.write"]>(node);
  return (
    <div className="space-y-4">
      <ValueExprInput
        label="ファイル名"
        text
        value={p.name as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { name: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: report.md"
        error={fieldErrors.get("name")}
      />
      <ValueExprInput
        label="内容"
        text
        value={p.content as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { content: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        error={fieldErrors.get("content")}
      />
      <FolderField {...props} label="保存先フォルダ（省略可）" optional hint="有効化のとき、このフォルダの編集権限を渡します" />
      <ContentTypeField {...props} />
    </div>
  );
}

export function StorageListForm(props: FormProps) {
  return (
    <FolderField {...props} label="一覧するフォルダ（省略可）" optional hint="有効化のとき、このフォルダの閲覧権限を渡します" />
  );
}

// ── CSV（csv.*）─────────────────────────────────────────────────────────

const CSV_PICKER = {
  accept: (n: NodeResponse) => isCsvFile(n),
  pickerTitle: "CSV ファイルを選ぶ",
  pickerDescription: "使う CSV ファイルを選んでください（.csv のファイルだけを表示しています）。",
};

/// 列の型（DuckDB の型名）の利用者向けの呼び名。
function typeLabel(t: string): string {
  const u = t.toUpperCase();
  if (/INT|DOUBLE|FLOAT|DECIMAL|NUMERIC|REAL/.test(u)) return "数値";
  if (/TIMESTAMP|DATE|TIME/.test(u)) return "日付";
  if (u.startsWith("BOOL")) return "真偽";
  return "文字";
}

/// CSV の列（本人の権限でスキーマを読む）とテーブル名の案内。
function CsvColumnsHint({ fileId }: { fileId: string | null }) {
  const [state, setState] = React.useState<{ id: string; schema: SchemaResponse | null } | null>(null);
  React.useEffect(() => {
    if (!fileId) return;
    let active = true;
    getSchema(fileId)
      .then((schema) => active && setState({ id: fileId, schema }))
      .catch(() => active && setState({ id: fileId, schema: null }));
    return () => {
      active = false;
    };
  }, [fileId]);
  const current = state && state.id === fileId ? state : null;
  return (
    <div className="space-y-1 rounded-md bg-muted/50 p-2 text-[11px] leading-relaxed text-muted-foreground" data-testid="csv-columns">
      <p>
        テーブル名は <code className="font-mono text-foreground">data</code> です。日本語や空白を含む列名は{" "}
        <code className="font-mono text-foreground">&quot;列名&quot;</code> のように二重引用符で囲みます。
      </p>
      {!fileId ? null : !current ? (
        <p>列を読み込み中…</p>
      ) : current.schema === null ? (
        <p className="text-destructive">列を読み取れませんでした（CSV として読めないか、閲覧の権限がありません）</p>
      ) : (
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
      )}
    </div>
  );
}

const PREVIEW_ROWS = 5;

/// 試し実行（本人の権限で SQL を流し、上位数行だけを見せる・ワークフローの実行や保存はしない）。
function CsvPreview({ fileId, sql }: { fileId: string | null; sql: string }) {
  const [running, setRunning] = React.useState(false);
  // 結果は流したときのファイルと SQL に紐づける（どちらかを変えたら古い結果を出さない）。
  const [result, setResult] = React.useState<{ key: string; table: TableResponse | null; error: string | null } | null>(
    null,
  );
  const key = `${fileId ?? ""}\n${sql}`;
  const shown = result && result.key === key ? result : null;
  const run = async () => {
    if (!fileId || !sql.trim()) return;
    setRunning(true);
    try {
      const table = await runQuery(fileId, sql);
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
          disabled={!fileId || !sql.trim() || running}
          onClick={() => void run()}
        >
          {!running ? <Play className="size-3.5" aria-hidden /> : null}
          試しに実行
        </Button>
        <span className="text-[11px] text-muted-foreground">
          {fileId ? "あなたの権限で流し、上位 5 行を表示します" : "ファイルを選ぶと試せます"}
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
  return (
    <div className="space-y-4">
      <FileField {...props} {...CSV_PICKER} label="集計する CSV ファイル" hint="有効化のとき、このファイルの閲覧権限を渡します" />
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
      <CsvColumnsHint fileId={fileId} />
      {sql !== null ? <CsvPreview fileId={fileId} sql={sql} /> : null}
    </div>
  );
}

/// 編集前の版（数で指定 ／ 前の結果から）。数のときは選んだファイルの今の版を入れられる。
function BaseRevField({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["csv.patch"]>(node);
  const value = p.base_rev as ValueExpr | undefined;
  const literal = typeof value === "number" ? value : null;
  const [mode, setMode] = React.useState<"number" | "expr">(value !== undefined && literal === null ? "expr" : "number");
  const fileId = typeof p.file === "string" && UUID_RE.test(p.file) ? p.file : null;
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const fillCurrent = async () => {
    if (!fileId) return;
    setLoading(true);
    setError(null);
    try {
      const n = await getNode(fileId);
      driveItems.set(n.id, n);
      patchParams(dispatch, node, { base_rev: n.version });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };
  return (
    <div className="space-y-1.5">
      {mode === "number" ? (
        <Field
          label="編集前の版"
          error={fieldErrors.get("base_rev") ?? error}
          hint="ほかの編集で版が進んでいると、競合として失敗します（固定の版は 1 回編集すると古くなります）"
        >
          <div className="flex items-center gap-1.5">
            <NumberInput
              value={literal}
              min={0}
              placeholder="例: 3"
              onChange={(n) => patchParams(dispatch, node, { base_rev: n ?? undefined })}
            />
            <Button
              variant="outline"
              size="sm"
              className="h-8 shrink-0 text-xs"
              disabled={!fileId || loading}
              loading={loading}
              onClick={() => void fillCurrent()}
            >
              今の版を入れる
            </Button>
          </div>
        </Field>
      ) : (
        <ValueExprInput
          label="編集前の版"
          value={value}
          onChange={(v) => patchParams(dispatch, node, { base_rev: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          error={fieldErrors.get("base_rev")}
        />
      )}
      <ModeToggle expr={mode === "expr"} pickLabel="数で指定する" onToggle={() => setMode(mode === "number" ? "expr" : "number")} />
    </div>
  );
}

export function CsvPatchForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const p = paramsOf<NodeParamsByType["csv.patch"]>(node);
  return (
    <div className="space-y-4">
      <FileField {...props} {...CSV_PICKER} label="編集する CSV ファイル" hint="有効化のとき、このファイルの編集権限を渡します" />
      <BaseRevField {...props} />
      <ValueExprInput
        label="編集の内容（操作の並び）"
        value={p.ops as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { ops: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        json="array"
        placeholder='例: [{"op": "row_insert", "at": 0, "values": ["2026-10-25", "交通費", "1200"]}]'
        error={fieldErrors.get("ops")}
      />
      <div className="space-y-0.5 text-[11px] leading-relaxed text-muted-foreground">
        <p>行・列の番号は 0 から数えます（見出しの行は数えません）。使える操作:</p>
        <ul className="list-disc pl-4 font-mono">
          <li>cell_update（row, col, value）</li>
          <li>row_insert（at, values）／ row_delete（row）</li>
          <li>column_add（name）／ column_delete（col）／ column_rename（col, name）</li>
        </ul>
      </div>
    </div>
  );
}

export function CsvWriteForm(props: FormProps) {
  const { node, dispatch, refCandidates, inMapRegion, fieldErrors } = props;
  const p = paramsOf<NodeParamsByType["csv.write"]>(node);
  return (
    <div className="space-y-4">
      <ValueExprInput
        label="ファイル名"
        text
        value={p.name as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { name: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: summary.csv（.csv は自動で付きます）"
        error={fieldErrors.get("name")}
      />
      <ValueExprInput
        label="CSV の内容"
        text
        value={p.content as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { content: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder={"例: 品目,金額\n交通費,1200"}
        error={fieldErrors.get("content")}
      />
      <p className="text-[11px] text-muted-foreground">1 行目は見出し（列名）です。前の結果を渡すときは CSV の文字列を渡してください。</p>
      <FolderField {...props} label="保存先フォルダ（省略可）" optional hint="有効化のとき、このフォルダの編集権限を渡します" />
    </div>
  );
}
