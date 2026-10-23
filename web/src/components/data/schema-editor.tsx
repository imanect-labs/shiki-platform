"use client";

/// スキーマエディタ（Task 15.5）: 列の追加・並べ替え・表示名の変更・索引のトグル。
///
/// - 表示名（日本語可）はいつでも変えられ、データは 1 行も動かない（参照名 `key` は不変・data-platform §4）。
/// - 列の追加・並べ替え・表示名の変更はまとめて「保存」でスキーマの改訂（楽観ロック）にする。
/// - 索引は列ごとに即時に付け外しする: 付けるのは昇格 API（2 万行未満は即時・それ以上は「準備中」と進捗）、
///   テーブルあたりの上限（既定 20）を超えるときは承認を求める（owner のみ・PIT-72）。外すのはスキーマの改訂。

import { ArrowDown, ArrowUp, Loader2, Plus, Sparkles } from "lucide-react";
import * as React from "react";

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
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/use-toast";
import {
  DataApiError,
  getTable,
  listIndexes,
  promoteIndex,
  updateSchema,
  type DataTable,
  type FieldDef,
  type FieldType,
  type TableIndexOverview,
} from "@/lib/data-api";
import { FIELD_TYPE_LABELS } from "@/lib/data-view";
import { cn } from "@/lib/utils";

/// 画面から追加できる型（派生列・リンクは別の画面で定義する）。
const ADDABLE_TYPES: FieldType[] = ["text", "number", "date", "date_time", "select", "multi_select"];

/// 表示名から参照名を作る（英数字だけなら小文字のスネークケース、そうでなければ連番）。
export function keyFromLabel(label: string, taken: ReadonlySet<string>): string {
  const ascii = label
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  let base = /^[a-z][a-z0-9_]*$/.test(ascii) ? ascii.slice(0, 48) : "col";
  if (base === "col" || taken.has(base)) {
    let i = 1;
    while (taken.has(`${base}_${i}`)) i += 1;
    base = `${base}_${i}`;
  }
  return base;
}

type Draft = FieldDef & { isNew?: boolean };

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
  const [adding, setAdding] = React.useState<{ label: string; type: FieldType; options: string } | null>(null);

  // 開いたときだけ下書きを作り直す（索引の付け外しでスキーマの版が進んでも、未保存の編集は消さない）。
  const tableRef = React.useRef(table);
  tableRef.current = table;
  React.useEffect(() => {
    if (!open) return;
    const t = tableRef.current;
    setFields([...t.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0)).map((f) => ({ ...f })));
    setAdding(null);
    listIndexes(t.id).then(setOverview).catch(() => setOverview(null));
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

  const dirty = React.useMemo(() => {
    const base = [...table.schema.fields].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
    if (base.length !== fields.length) return true;
    return fields.some((f, i) => f.key !== base[i]?.key || (f.display_name ?? "") !== (base[i]?.display_name ?? ""));
  }, [fields, table]);

  const move = (i: number, d: -1 | 1) => {
    const j = i + d;
    if (j < 0 || j >= fields.length) return;
    setFields((fs) => {
      const n = [...fs];
      [n[i], n[j]] = [n[j], n[i]];
      return n;
    });
  };

  const save = async () => {
    setSaving(true);
    try {
      // 索引の状態は最新のスキーマに従う（このシートで付け外しした分も反映済み）。
      const latest = new Map(table.schema.fields.map((f) => [f.key, f]));
      const next = fields.map(({ isNew, ...f }, i) => {
        void isNew;
        const cur = latest.get(f.key);
        return { ...(cur ?? f), display_name: f.display_name, order: i };
      });
      const saved = await updateSchema(table.id, { ...table.schema, fields: next }, table.schema_version);
      toast({ title: "列の設定を保存しました" });
      onSaved(saved);
      onOpenChange(false);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "保存できませんでした",
        description:
          e instanceof DataApiError && e.status === 409
            ? "他の人がスキーマを変更しました。開き直してからもう一度お試しください。"
            : e instanceof Error
              ? e.message
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

  const taken = new Set(fields.map((f) => f.key));
  const addField = () => {
    if (!adding || !adding.label.trim()) return;
    const key = keyFromLabel(adding.label, taken);
    const options = adding.options
      .split(/[,、\n]/)
      .map((s) => s.trim())
      .filter(Boolean);
    const f: Draft = {
      key,
      display_name: adding.label.trim(),
      type: adding.type,
      options: adding.type === "select" || adding.type === "multi_select" ? options : [],
      isNew: true,
    };
    setFields((fs) => [...fs, f]);
    setAdding(null);
  };
  const needsOptions = adding?.type === "select" || adding?.type === "multi_select";

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
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
          <ul className="divide-y rounded-lg border">
            {fields.map((f, i) => {
              const derived = ["lookup", "rollup", "formula", "computed"].includes(f.type);
              const canIndex = !f.isNew && !(derived && f.materialization !== "invariant") && f.type !== "record_ref";
              return (
                <li key={f.key} className="flex items-center gap-2 px-2.5 py-2" data-testid={`schema-field-${f.key}`}>
                  <div className="flex flex-col">
                    <button
                      type="button"
                      aria-label={`「${f.display_name || f.key}」を上へ`}
                      disabled={i === 0}
                      onClick={() => move(i, -1)}
                      className="rounded px-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <ArrowUp className="size-3" aria-hidden />
                    </button>
                    <button
                      type="button"
                      aria-label={`「${f.display_name || f.key}」を下へ`}
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
                      value={f.display_name ?? ""}
                      placeholder={f.key}
                      onChange={(e) =>
                        setFields((fs) => fs.map((x, j) => (j === i ? { ...x, display_name: e.target.value } : x)))
                      }
                      className="h-8 text-[13px]"
                    />
                    <p className="mt-0.5 flex items-center gap-1.5 truncate text-[11px] text-muted-foreground">
                      <span className="font-mono">{f.key}</span>·<span>{FIELD_TYPE_LABELS[f.type]}</span>
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
                          aria-label={`「${f.display_name || f.key}」の索引`}
                          checked={Boolean(f.indexed) || overview?.fields.some((x) => x.field === f.key && x.status !== "absent")}
                          disabled={!canIndex || busyIndex !== null}
                          onCheckedChange={(on) => void toggleIndex(f, on)}
                        />
                      )}
                    </label>
                    <IndexState field={f} overview={overview} />
                  </div>
                </li>
              );
            })}
          </ul>

          {adding ? (
            <div className="space-y-2.5 rounded-lg border border-primary/30 bg-primary/[0.03] p-3" data-testid="schema-add-form">
              <div className="grid grid-cols-[1fr_9rem] gap-2">
                <Input
                  autoFocus
                  aria-label="新しい列の表示名"
                  placeholder="列の名前（例: 金額）"
                  value={adding.label}
                  onChange={(e) => setAdding({ ...adding, label: e.target.value })}
                  onKeyDown={(e) => e.key === "Enter" && !needsOptions && addField()}
                  className="h-8 text-[13px]"
                />
                <Select value={adding.type} onValueChange={(t) => setAdding({ ...adding, type: t as FieldType })}>
                  <SelectTrigger aria-label="新しい列の型" className="h-8 text-[13px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {ADDABLE_TYPES.map((t) => (
                      <SelectItem key={t} value={t}>
                        {FIELD_TYPE_LABELS[t]}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              {needsOptions ? (
                <Input
                  aria-label="選択肢"
                  placeholder="選択肢をカンマ区切りで（例: 申請中, 承認, 却下）"
                  value={adding.options}
                  onChange={(e) => setAdding({ ...adding, options: e.target.value })}
                  onKeyDown={(e) => e.key === "Enter" && addField()}
                  className="h-8 text-[13px]"
                />
              ) : null}
              <div className="flex justify-end gap-2">
                <Button variant="ghost" size="sm" className="h-8 text-xs" onClick={() => setAdding(null)}>
                  やめる
                </Button>
                <Button
                  size="sm"
                  className="h-8 text-xs"
                  disabled={!adding.label.trim() || (needsOptions && !adding.options.trim())}
                  onClick={addField}
                  data-testid="schema-add-confirm"
                >
                  追加
                </Button>
              </div>
            </div>
          ) : (
            <Button
              variant="outline"
              size="sm"
              className="w-full border-dashed"
              onClick={() => setAdding({ label: "", type: "text", options: "" })}
              data-testid="schema-add-field"
            >
              <Plus className="size-4" aria-hidden />
              列を追加
            </Button>
          )}
        </SheetBody>
        <div className="flex items-center justify-end gap-2 border-t px-4 py-3">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            閉じる
          </Button>
          <Button onClick={save} disabled={!dirty || saving} data-testid="schema-save">
            {saving ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            保存
          </Button>
        </div>
      </SheetContent>

      <Dialog open={approval !== null} onOpenChange={(o) => !o && setApproval(null)}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Sparkles className="size-4 text-primary" aria-hidden />
              索引の上限を超えます
            </DialogTitle>
            <DialogDescription>
              このテーブルの索引は {approval?.indexed ?? 0} 列で、上限は {approval?.cap ?? 0} 列です。
              索引を増やすと書き込みが少し重くなります。承認して「{approval?.field}」の索引を作りますか？
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
    </Sheet>
  );
}
