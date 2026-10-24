"use client";

/// レコードの追加フォーム（Task 15.5）。直接入力できる列（テキスト・数値・日付・選択）と、候補から選ぶ参照の列
/// （ユーザー・部署・リンク・A-6）を並べ、値の検査は画面でも行う（正はサーバの検証器・不合格は理由を表示）。

import { Loader2 } from "lucide-react";
import * as React from "react";

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
import { toast } from "@/components/ui/use-toast";
import { isRefField, RefPicker } from "@/components/data/ref-picker";
import { createRecord, DataApiError, type DataRecord, type DataTable } from "@/lib/data-api";
import { isEditable, parseInput } from "@/lib/data-view";

const NONE = "__none__";

export function RecordDialog({
  open,
  onOpenChange,
  table,
  masked,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  table: DataTable;
  masked: ReadonlySet<string>;
  onCreated: (r: DataRecord) => void;
}) {
  const fields = React.useMemo(
    () =>
      [...table.schema.fields]
        .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
        .filter((f) => isEditable(f) && !masked.has(f.key)),
    [table, masked],
  );
  const [values, setValues] = React.useState<Record<string, string>>({});
  /// 参照の列で選んだ値（id）。
  const [refs, setRefs] = React.useState<Record<string, string[]>>({});
  const [errors, setErrors] = React.useState<Record<string, string>>({});
  const [busy, setBusy] = React.useState(false);
  React.useEffect(() => {
    if (open) {
      setValues({});
      setRefs({});
      setErrors({});
    }
  }, [open]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const data: Record<string, unknown> = {};
    const errs: Record<string, string> = {};
    for (const f of fields) {
      const picked = refs[f.key];
      const raw = isRefField(f) ? (picked?.length ? JSON.stringify({ ids: picked }) : "") : (values[f.key] ?? "");
      const p = parseInput(raw, f);
      if (!p.ok) errs[f.key] = p.error;
      else if (p.value !== null) data[f.key] = p.value;
      else if (f.required) errs[f.key] = "必須の項目です";
    }
    setErrors(errs);
    if (Object.keys(errs).length) return;
    setBusy(true);
    try {
      const r = await createRecord(table.id, data);
      toast({ title: "レコードを追加しました" });
      onCreated(r);
      onOpenChange(false);
    } catch (err) {
      // 列に結び付いた理由（構造化 400 の field）はその列の下に出す。
      const field = err instanceof DataApiError ? err.field : null;
      if (field && fields.some((f) => f.key === field)) {
        setErrors({ [field]: (err as Error).message });
        return;
      }
      toast({
        variant: "destructive",
        title: "追加できませんでした",
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md" data-testid="record-dialog">
        <form onSubmit={submit} className="space-y-4" noValidate>
          <DialogHeader>
            <DialogTitle>レコードを追加</DialogTitle>
            <DialogDescription>{table.name} に 1 件追加します。</DialogDescription>
          </DialogHeader>
          <div className="max-h-[55dvh] space-y-3 overflow-y-auto px-0.5 scrollbar-subtle">
            {fields.length === 0 ? (
              <p className="text-sm text-muted-foreground">直接入力できる列がありません。</p>
            ) : null}
            {fields.map((f) => {
              const id = `rec-${f.key}`;
              const label = f.display_name || f.key;
              return (
                <div key={f.key} className="space-y-1.5">
                  <label htmlFor={id} className="text-xs font-medium">
                    {label}
                    {f.required ? <span className="ml-0.5 text-destructive">*</span> : null}
                  </label>
                  {isRefField(f) ? (
                    <RefPicker
                      id={id}
                      field={f}
                      value={refs[f.key] ?? []}
                      onChange={(ids) => setRefs((s) => ({ ...s, [f.key]: ids }))}
                    />
                  ) : f.type === "select" ? (
                    <Select
                      value={values[f.key] || NONE}
                      onValueChange={(v) => setValues((s) => ({ ...s, [f.key]: v === NONE ? "" : v }))}
                    >
                      <SelectTrigger id={id} className="h-9">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value={NONE}>（未選択）</SelectItem>
                        {(f.options ?? []).map((o) => (
                          <SelectItem key={o} value={o}>
                            {o}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  ) : (
                    <Input
                      id={id}
                      type={f.type === "date" ? "date" : f.type === "date_time" ? "datetime-local" : "text"}
                      inputMode={f.type === "number" ? "decimal" : undefined}
                      value={values[f.key] ?? ""}
                      onChange={(e) => setValues((s) => ({ ...s, [f.key]: e.target.value }))}
                      placeholder={f.type === "multi_select" ? (f.options ?? []).join(", ") : undefined}
                      aria-invalid={Boolean(errors[f.key])}
                      className="h-9"
                    />
                  )}
                  {errors[f.key] ? <p className="text-[11px] text-destructive">{errors[f.key]}</p> : null}
                </div>
              );
            })}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              キャンセル
            </Button>
            <Button type="submit" disabled={busy} data-testid="record-dialog-submit">
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              追加
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
