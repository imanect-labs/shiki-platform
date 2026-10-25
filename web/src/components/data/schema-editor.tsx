"use client";

/// スキーマエディタ（Task 15.5・A-5）: 列の追加（全型）・属性の編集・並べ替え・表示名の変更・索引のトグル。
///
/// - 表示名（日本語可）はいつでも変えられ、データは 1 行も動かない（参照名 `key` は不変・data-platform §4）。
/// - 列の追加・並べ替え・表示名と属性の変更はまとめて「保存」でスキーマの改訂（楽観ロック）にする。
///   保存していない変更があるまま閉じようとしたら確かめる。
/// - スキーマの改訂は追加のみ（data-platform §4・`validate_schema_update`）: 保存済みの列の削除・型・参照先・
///   派生の定義・数値の桁の変更はできない。保存前に足した列だけを取り消せる（使わない列はビューで隠す）。
/// - 索引は列ごとに即時に付け外しする: 付けるのは昇格 API（2 万行未満は即時・それ以上は「準備中」と進捗）、
///   テーブルあたりの上限（既定 20）を超えるときは承認を求める（owner のみ・PIT-72）。外すのはスキーマの改訂。
/// - テーブル自体の削除もここから（owner のみ・確かめてから）。

import { AlertCircle, ArrowDown, ArrowUp, ChevronDown, Loader2, Plus, Sparkles, Trash2, X } from "lucide-react";
import { useRouter } from "next/navigation";
import * as React from "react";

import { ConfirmDialog } from "@/components/drive/dialogs";
import {
  AddFieldForm,
  FieldSettings,
  isDerived,
  type TableTraits,
} from "@/components/data/schema-field-form";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import {
  DataApiError,
  deleteTable,
  getTable,
  listIndexes,
  listTables,
  promoteIndex,
  updateSchema,
  type DataTable,
  type FieldDef,
  type TableIndexOverview,
} from "@/lib/data-api";
import { FIELD_TYPE_LABELS, withFieldLabels } from "@/lib/data-view";
import { cn } from "@/lib/utils";

type Draft = FieldDef & { isNew?: boolean };

/// 比較用の正規化（保存の要否の判定・空の属性の有無の揺れを吸収）。索引はこのシートで即時に付け外しするので
/// 比べない。
function normalize(f: FieldDef) {
  return {
    key: f.key,
    display_name: f.display_name ?? "",
    description: f.description ?? null,
    required: Boolean(f.required),
    unique: Boolean(f.unique),
    searchable: Boolean(f.searchable),
    options: f.options ?? [],
    numeric_spec: f.numeric_spec ?? null,
  };
}

function IndexState({
  field,
  overview,
}: {
  field: FieldDef;
  overview: TableIndexOverview | null;
}) {
  const st = overview?.fields.find((f) => f.field === field.key)?.status;
  if (st === "preparing") {
    const p = overview?.backfill_progress;
    return (
      <Badge variant="warning" className="gap-1" data-testid={`index-preparing-${field.key}`}>
        <Loader2 className="size-3 animate-spin" aria-hidden />
        準備中{typeof p === "number" ? ` ${Math.round(p * 100)}%` : ""}
      </Badge>
    );
  }
  if (st === "active") return <Badge variant="success">有効</Badge>;
  return null;
}

export function SchemaEditor({
  open,
  onOpenChange,
  table,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  table: DataTable;
  onSaved: (t: DataTable) => void;
}) {
  const [fields, setFields] = React.useState<Draft[]>([]);
  const [overview, setOverview] = React.useState<TableIndexOverview | null>(null);
  const [saving, setSaving] = React.useState(false);
  const [busyIndex, setBusyIndex] = React.useState<string | null>(null);
  const [approval, setApproval] = React.useState<{ field: string; indexed: number; cap: number } | null>(null);
  const [adding, setAdding] = React.useState(false);
  const [expanded, setExpanded] = React.useState<string | null>(null);
  const [confirmClose, setConfirmClose] = React.useState(false);
  const [confirmDeleteTable, setConfirmDeleteTable] = React.useState(false);
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  /// 今のデータと合わない設定で保存できなかった理由（400 `schema_conflicts_with_data`・列の表示名と件数つきの
  /// サーバの文言）。その列の下に出し、列の設定を変えたら消す。
  const [dataConflict, setDataConflict] = React.useState<{ field: string | null; message: string } | null>(null);
  const router = useRouter();
  // 属性の警告の材料（行ポリシー・マスク。owner には定義が返る）。
  const traits: TableTraits = React.useMemo(
    () => ({
      rowPolicy: Boolean(table.schema.row_policy),
      masked: new Set((table.schema.field_policy ?? []).map((p) => p.field)),
    }),
    [table.schema],
  );

  // 開いたときだけ下書きを作り直す（索引の付け外しでスキーマの版が進んでも、未保存の編集は消さない）。
  const tableRef = React.useRef(table);
  tableRef.current = table;
  React.useEffect(() => {
    if (!open) return;
    const t = tableRef.current;
    setFields([...t.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0)).map((f) => ({ ...f })));
    setAdding(false);
    setExpanded(null);
    setDataConflict(null);
    listIndexes(t.id).then(setOverview).catch(() => setOverview(null));
    // リンクの参照先の名前（定義の要約）に使う。
    listTables()
      .then(setTables)
      .catch(() => setTables(null));
  }, [open]);

  // 準備中の索引があれば進捗を読み直す（完了したら有効になる）。
  const preparing = overview?.fields.some((f) => f.status === "preparing") ?? false;
  React.useEffect(() => {
    if (!open || !preparing) return;
    const t = window.setInterval(() => {
      listIndexes(table.id).then(setOverview).catch(() => undefined);
    }, 2000);
    return () => window.clearInterval(t);
  }, [open, preparing, table.id]);

  const kept = fields;
  const dirty = React.useMemo(() => {
    const base = [...table.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
    return JSON.stringify(base.map(normalize)) !== JSON.stringify(kept.map(normalize));
  }, [kept, table]);

  /// 閉じる: 保存していない変更があれば確かめる（黙って捨てない）。
  const requestClose = (o: boolean) => {
    if (o) return onOpenChange(true);
    if (dirty && !saving) setConfirmClose(true);
    else onOpenChange(false);
  };

  const move = (i: number, d: -1 | 1) => {
    const j = i + d;
    if (j < 0 || j >= fields.length) return;
    setFields((fs) => {
      const n = [...fs];
      [n[i], n[j]] = [n[j], n[i]];
      return n;
    });
  };

  // 表示名を空にした列（元は表示名があった列）。空のまま保存すると見出しが参照名（col_4 など）になる。
  const emptyNames = React.useMemo(() => {
    const before = new Map(table.schema.fields.map((f) => [f.key, f.display_name ?? ""]));
    return new Set(
      kept.filter((f) => !(f.display_name ?? "").trim() && (f.isNew || (before.get(f.key) ?? "").trim())).map((f) => f.key),
    );
  }, [kept, table]);

  const save = async () => {
    if (emptyNames.size) return;
    setSaving(true);
    try {
      // 索引の状態は最新のスキーマに従う（このシートで付け外しした分も反映済み）。
      const latest = new Map(table.schema.fields.map((f) => [f.key, f]));
      const next = kept.map(({ isNew, ...f }, i) => {
        void isNew;
        const cur = latest.get(f.key);
        // 既存の列は最新の定義（索引の状態を含む）に、このシートで変えた属性だけを重ねる。
        return cur
          ? {
              ...cur,
              display_name: f.display_name,
              description: f.description ?? null,
              required: f.required,
              unique: f.unique,
              searchable: f.searchable,
              options: f.options,
              numeric_spec: f.numeric_spec ?? null,
              order: i,
            }
          : { ...f, order: i };
      });
      const saved = await updateSchema(table.id, { ...table.schema, fields: next }, table.schema_version);
      setDataConflict(null);
      toast({ title: "列の設定を保存しました" });
      onSaved(saved);
      onOpenChange(false);
    } catch (e) {
      // 誤りの列はサーバが参照名で名指しする（400 の field・409 unique_violation の field）。
      const field = e instanceof DataApiError ? e.field : null;
      const fieldLabel = field ? fields.find((f) => f.key === field)?.display_name || field : null;
      if (e instanceof DataApiError && e.code === "schema_conflicts_with_data") {
        // 今のデータと合わない（値の無い行がある列の必須化・使われている選択肢の削除）: サーバの文言（列の
        // 表示名と件数つき）をその列の下に出し、列の設定を開く。
        setDataConflict({ field, message: withFieldLabels(e.message, fields) });
        if (field) setExpanded(field);
        return;
      }
      toast({
        variant: "destructive",
        title: fieldLabel ? `「${fieldLabel}」を保存できませんでした` : "保存できませんでした",
        description:
          e instanceof DataApiError && e.uniqueViolation
            ? `既存のレコードに同じ値があるため、${fieldLabel ? `「${fieldLabel}」` : "この列"}を一意にできません。重複を直してからもう一度保存してください。`
            : e instanceof DataApiError && e.status === 409 && (e.code === "conflict" || e.code === "rev_conflict" || e.code === null)
              ? "他の人がスキーマを変更しました。開き直してからもう一度お試しください。"
              : e instanceof Error
                ? withFieldLabels(e.message, fields)
                : String(e),
      });
    } finally {
      setSaving(false);
    }
  };

  const toggleIndex = async (f: FieldDef, on: boolean, approve = false) => {
    setBusyIndex(f.key);
    try {
      if (on) {
        const r = await promoteIndex(table.id, f.key, approve);
        // 昇格はスキーマの改訂（indexed）を伴うので、版を取り直して以後の保存の楽観ロックを合わせる。
        onSaved(await getTable(table.id));
        toast({
          title: r.status === "active" ? `「${f.display_name || f.key}」で並べ替え・絞り込みできます` : "索引を準備しています",
          description: r.status === "active" ? undefined : "大きなテーブルのため裏で作っています。完了まで「準備中」と表示します。",
        });
      } else {
        const next = table.schema.fields.map((x) => (x.key === f.key ? { ...x, indexed: false } : x));
        const saved = await updateSchema(table.id, { ...table.schema, fields: next }, table.schema_version);
        onSaved(saved);
      }
      setOverview(await listIndexes(table.id));
      setFields((fs) => fs.map((x) => (x.key === f.key ? { ...x, indexed: on } : x)));
    } catch (e) {
      const need = e instanceof DataApiError ? e.approvalRequired : null;
      if (need) {
        setApproval(need);
      } else {
        toast({
          variant: "destructive",
          title: on ? "索引を有効にできませんでした" : "索引を外せませんでした",
          description: e instanceof Error ? e.message : String(e),
        });
      }
    } finally {
      setBusyIndex(null);
    }
  };

  const patchField = (key: string, f: FieldDef) => {
    setFields((fs) => fs.map((x) => (x.key === key ? { ...x, ...f } : x)));
    setDataConflict((c) => (c && (c.field === key || c.field === null) ? null : c));
  };
  /// 保存前に足した列を取り消す（保存済みの列は消せない）。足した列を使う派生列も一緒に取り消す。
  const removeNew = (f: Draft) => {
    const uses = (x: Draft) =>
      x.lookup?.via_field === f.key || x.rollup?.link === f.key || JSON.stringify(x.formula ?? null).includes(`"field":"${f.key}"`);
    setFields((fs) => fs.filter((x) => x.key !== f.key && !(x.isNew && uses(x))));
    if (expanded === f.key) setExpanded(null);
  };

  return (
    <Sheet open={open} onOpenChange={requestClose}>
      <SheetContent side="right" className="w-full sm:max-w-lg" data-testid="schema-editor">
        <SheetHeader className="pr-14">
          <SheetTitle>列の編集</SheetTitle>
          <SheetDescription>
            表示名はいつでも変えられます（データはそのまま）。索引を付けた列で並べ替え・絞り込みできます。
          </SheetDescription>
        </SheetHeader>
        <SheetBody className="space-y-4 px-4 pb-4">
          {overview ? (
            <p className="text-[11px] text-muted-foreground">
              索引 {overview.indexed_fields} / {overview.max_indexed_fields} 列
            </p>
          ) : null}
          {dataConflict && (dataConflict.field === null || !fields.some((f) => f.key === dataConflict.field)) ? (
            <DataConflictNote message={dataConflict.message} />
          ) : null}
          <ul className="divide-y rounded-lg border">
            {fields.map((f, i) => {
              const derived = isDerived(f.type);
              const reverse = Boolean(f.link?.reverse);
              const canIndex =
                !f.isNew && !(derived && f.materialization !== "invariant") && f.type !== "record_ref";
              const name = f.display_name || f.key;
              return (
                <li key={f.key} data-testid={`schema-field-${f.key}`}>
                  <div className="flex items-center gap-2 px-2.5 py-2">
                    <div className="flex flex-col">
                      <button
                        type="button"
                        aria-label={`「${name}」を上へ`}
                        disabled={i === 0}
                        onClick={() => move(i, -1)}
                        className="rounded px-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        <ArrowUp className="size-3" aria-hidden />
                      </button>
                      <button
                        type="button"
                        aria-label={`「${name}」を下へ`}
                        disabled={i === fields.length - 1}
                        onClick={() => move(i, 1)}
                        className="rounded px-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        <ArrowDown className="size-3" aria-hidden />
                      </button>
                    </div>
                    <div className="min-w-0 flex-1">
                      <Input
                        aria-label={`「${f.key}」の表示名`}
                        aria-invalid={emptyNames.has(f.key) || undefined}
                        aria-describedby={emptyNames.has(f.key) ? `name-error-${f.key}` : undefined}
                        value={f.display_name ?? ""}
                        placeholder="表示名"
                        maxLength={100}
                        onChange={(e) => patchField(f.key, { ...f, display_name: e.target.value })}
                        className={cn("h-8 text-[13px]", emptyNames.has(f.key) && "border-destructive focus-visible:ring-destructive/40")}
                      />
                      {emptyNames.has(f.key) ? (
                        <p id={`name-error-${f.key}`} className="mt-0.5 text-[11px] text-destructive" role="alert">
                          表示名を入れてください（空にはできません）
                        </p>
                      ) : null}
                      <p className="mt-0.5 flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                        <span className="font-mono">{f.key}</span>·<span>{FIELD_TYPE_LABELS[f.type]}</span>
                        {f.required ? <Badge variant="muted">必須</Badge> : null}
                        {f.unique ? <Badge variant="muted">一意</Badge> : null}
                        {f.searchable ? <Badge variant="muted">検索</Badge> : null}
                        {reverse ? <Badge variant="muted">逆向き</Badge> : null}
                        {f.isNew ? <Badge variant="secondary">未保存</Badge> : null}
                      </p>
                    </div>
                    <div className="flex shrink-0 flex-col items-end gap-1">
                      <label className={cn("flex items-center gap-1.5 text-[11px] text-muted-foreground", !canIndex && "opacity-50")}>
                        索引
                        {busyIndex === f.key ? (
                          <Loader2 className="size-3.5 animate-spin" aria-hidden />
                        ) : (
                          <Switch
                            aria-label={`「${name}」の索引`}
                            checked={Boolean(f.indexed || overview?.fields.some((x) => x.field === f.key && x.status !== "absent"))}
                            disabled={!canIndex || busyIndex !== null}
                            onCheckedChange={(on) => void toggleIndex(f, on)}
                          />
                        )}
                      </label>
                      <IndexState field={f} overview={overview} />
                    </div>
                    <div className="flex shrink-0 items-center">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-7 text-muted-foreground"
                        aria-label={`「${name}」の設定`}
                        aria-expanded={expanded === f.key}
                        onClick={() => setExpanded((k) => (k === f.key ? null : f.key))}
                      >
                        <ChevronDown className={cn("size-3.5 transition-transform", expanded === f.key && "rotate-180")} aria-hidden />
                      </Button>
                      {f.isNew ? (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-7 text-muted-foreground hover:text-destructive"
                          aria-label={`「${name}」の追加を取り消す`}
                          onClick={() => removeNew(f)}
                        >
                          <X className="size-3.5" aria-hidden />
                        </Button>
                      ) : null}
                    </div>
                  </div>
                  {dataConflict && dataConflict.field === f.key ? (
                    <DataConflictNote message={dataConflict.message} />
                  ) : null}
                  {expanded === f.key ? (
                    <FieldSettings
                      field={f}
                      fields={kept}
                      tables={tables}
                      traits={traits}
                      onChange={(nf) => patchField(f.key, nf)}
                    />
                  ) : null}
                </li>
              );
            })}
          </ul>
          <p className="text-[11px] text-muted-foreground">
            保存した列は削除・型の変更ができません（データを守るため）。使わない列はビューの「列」で隠せます。
          </p>

          {adding ? (
            <AddFieldForm
              fields={kept}
              tableId={table.id}
              tableName={table.name}
              traits={traits}
              onCancel={() => setAdding(false)}
              onAdd={(f) => {
                setFields((fs) => [...fs, { ...f, isNew: true }]);
                setAdding(false);
              }}
            />
          ) : (
            <Button
              variant="outline"
              size="sm"
              className="w-full border-dashed"
              onClick={() => setAdding(true)}
              data-testid="schema-add-field"
            >
              <Plus className="size-4" aria-hidden />
              列を追加
            </Button>
          )}
        </SheetBody>
        <div className="flex items-center gap-2 border-t px-4 py-3">
          <Button
            variant="ghost"
            className="mr-auto text-destructive hover:bg-destructive/10 hover:text-destructive"
            onClick={() => setConfirmDeleteTable(true)}
            data-testid="table-delete"
          >
            <Trash2 className="size-4" aria-hidden />
            <span className="max-sm:sr-only">テーブルを削除</span>
          </Button>
          <Button variant="outline" onClick={() => requestClose(false)}>
            閉じる
          </Button>
          <Button onClick={save} disabled={!dirty || saving || emptyNames.size > 0} data-testid="schema-save">
            {saving ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            保存
          </Button>
        </div>
        {/* 確かめるダイアログはシートの中に置く（外に置くと、閉じた後にシートの Esc・外側の操作の扱いがずれる）。 */}
        <ConfirmDialog
          open={confirmClose}
          onOpenChange={setConfirmClose}
          title="保存していない変更があります"
          description="列の追加・削除・設定の変更を保存せずに閉じますか？保存していない変更は失われます。"
          confirmLabel="変更を捨てて閉じる"
          destructive
          onConfirm={async () => {
            onOpenChange(false);
          }}
        />
        <ConfirmDialog
          open={confirmDeleteTable}
          onOpenChange={setConfirmDeleteTable}
          title="テーブルを削除しますか？"
          description={`「${table.name}」とそのレコード・ビューを削除します。元に戻せません。このテーブルを使うワークフローやアプリは読めなくなります。`}
          confirmLabel="削除する"
          destructive
          onConfirm={async () => {
            await deleteTable(table.id);
            toast({ title: `「${table.name}」を削除しました` });
            onOpenChange(false);
            router.push("/data");
          }}
        />
        <Dialog open={approval !== null} onOpenChange={(o) => !o && setApproval(null)}>
          <DialogContent className="max-w-sm">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <Sparkles className="size-4 text-primary" aria-hidden />
                索引の上限を超えます
              </DialogTitle>
              <DialogDescription>
                このテーブルの索引は {approval?.indexed ?? 0} 列で、上限は {approval?.cap ?? 0} 列です。索引を増やすと書き込みが少し重くなります。承認して「{fields.find((x) => x.key === approval?.field)?.display_name || approval?.field}」の索引を作りますか？
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button variant="outline" onClick={() => setApproval(null)}>
                やめる
              </Button>
              <Button
                onClick={() => {
                  const f = fields.find((x) => x.key === approval?.field);
                  setApproval(null);
                  if (f) void toggleIndex(f, true, true);
                }}
              >
                承認して作る
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </SheetContent>

    </Sheet>
  );
}

/// 今のデータと合わないため保存できなかった理由（サーバの文言・列の表示名と件数つき）。
function DataConflictNote({ message }: { message: string }) {
  return (
    <p
      role="alert"
      data-testid="schema-data-conflict"
      className="mx-2.5 mb-2 flex items-start gap-1.5 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-2 text-[12px] leading-relaxed text-destructive"
    >
      <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
      <span>
        {message}
        <span className="block text-[11px] text-muted-foreground">データを直すか、設定を戻してからもう一度保存してください。</span>
      </span>
    </p>
  );
}
