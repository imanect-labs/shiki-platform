"use client";

/// CSV からテーブルを作る: 1 行目を列名、2 行目以降をデータとして読み、列ごとの型を推定して見せる。
/// 列名・型を直して「作成」を押すと、全列を持つテーブルを作り、データの行を一括取込（§16.7）で入れる。
///
/// CSV はブラウザで読み（UTF-8。読めなければ Excel の既定の Shift_JIS として読む）、見出しを参照名に
/// 置き換えた UTF-8 の CSV を取込に渡す。型・必須などの検証は取込のジョブが行い、不合格の行は
/// テーブル画面の取込の結果に出る。

import { FileUp, Loader2 } from "lucide-react";
import * as React from "react";

import { writeStored } from "@/components/data/import-dialog";
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
import { parseCsv } from "@/lib/csv/parse";
import { createTable, DataApiError, startCsvImport, type DataTable } from "@/lib/data-api";
import {
  buildCsvTable,
  CSV_COLUMN_TYPES,
  MAX_CSV_BYTES,
  planFromCsv,
  tableNameFromFile,
  type CsvColumnType,
  type CsvTablePlan,
} from "@/lib/data-csv";
import { cn } from "@/lib/utils";

const fmt = (n: number) => n.toLocaleString("ja-JP");

/// バイト列を文字列にする（UTF-8 → 読めなければ Shift_JIS）。先頭の BOM は除く。
export function decodeCsv(buf: ArrayBuffer): string {
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(buf);
  } catch {
    try {
      text = new TextDecoder("shift_jis", { fatal: true }).decode(buf);
    } catch {
      throw new Error("文字コードを読めませんでした。UTF-8 か Shift_JIS の CSV で保存し直してください。");
    }
  }
  return text.replace(/^﻿/, "");
}

export function CreateFromCsvDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onCreated: (t: DataTable) => void;
}) {
  const [fileName, setFileName] = React.useState<string | null>(null);
  const [plan, setPlan] = React.useState<CsvTablePlan | null>(null);
  const [name, setName] = React.useState("");
  const [nameError, setNameError] = React.useState<string | null>(null);
  const [fileError, setFileError] = React.useState<string | null>(null);
  const [reading, setReading] = React.useState(false);
  const [busy, setBusy] = React.useState(false);
  const inputRef = React.useRef<HTMLInputElement>(null);
  /// 読込の世代（選び直し・開き直しで進める。古い読込の結果は捨てる）。
  const readSeq = React.useRef(0);

  React.useEffect(() => {
    readSeq.current += 1;
    if (open) {
      setFileName(null);
      setPlan(null);
      setName("");
      setNameError(null);
      setFileError(null);
    }
  }, [open]);

  const pick = async (file: File | undefined) => {
    if (!file) return;
    const seq = ++readSeq.current;
    setFileError(null);
    setPlan(null);
    setFileName(file.name);
    if (file.size > MAX_CSV_BYTES) {
      setFileError(`ファイルが大きすぎます（最大 ${MAX_CSV_BYTES / (1024 * 1024)} MB）。`);
      setReading(false);
      return;
    }
    setReading(true);
    try {
      const p = planFromCsv(parseCsv(decodeCsv(await file.arrayBuffer())));
      if (seq !== readSeq.current) return;
      setPlan(p);
      setName((n) => n || tableNameFromFile(file.name));
    } catch (e) {
      if (seq !== readSeq.current) return;
      setFileError(e instanceof Error ? e.message : String(e));
    } finally {
      if (seq === readSeq.current) setReading(false);
    }
  };

  const setColumn = (i: number, patch: { name?: string; type?: CsvColumnType }) =>
    setPlan((p) => (p ? { ...p, columns: p.columns.map((c, j) => (j === i ? { ...c, ...patch } : c)) } : p));

  const emptyColumn = plan?.columns.findIndex((c) => !c.name.trim()) ?? -1;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!plan || !name.trim() || emptyColumn >= 0) return;
    setBusy(true);
    let table: DataTable;
    const { fields, csv } = buildCsvTable(plan);
    // 改行の CRLF 化・Shift_JIS からの変換で、選んだファイルより送る CSV が大きくなることがある。
    // 取込が 413 で断る大きさなら、空のテーブルを作る前に止める。
    if (new Blob([csv]).size > MAX_CSV_BYTES) {
      setFileError(
        `UTF-8 に直すと ${MAX_CSV_BYTES / (1024 * 1024)} MB を超えるため取り込めません。行を分けて作成してください。`,
      );
      setBusy(false);
      return;
    }
    try {
      table = await createTable(name.trim(), { fields, field_policy: [] });
    } catch (err) {
      if (err instanceof DataApiError && err.nameTaken) {
        setNameError("同じ名前のテーブルがあります。別の名前にしてください。");
      } else if (err instanceof DataApiError && err.status === 400 && err.field === "name") {
        setNameError(err.message);
      } else {
        toast({
          variant: "destructive",
          title: "テーブルを作れませんでした",
          description: err instanceof Error ? err.message : String(err),
        });
      }
      setBusy(false);
      return;
    }
    try {
      const r = await startCsvImport(table.id, csv);
      // テーブル画面の取込の表示が同じ取込の進捗と結果を追う。
      writeStored(table.id, r.import_id);
    } catch (err) {
      // テーブルはできている。行はテーブル画面の「取り込む」から入れ直せる。
      toast({
        variant: "destructive",
        title: "テーブルは作りましたが、行を取り込めませんでした",
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
    onCreated(table);
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !busy && onOpenChange(o)}>
      <DialogContent className="max-w-2xl" data-testid="create-from-csv-dialog">
        <form onSubmit={submit} className="space-y-4">
          <DialogHeader>
            <DialogTitle>CSV からテーブルを作成</DialogTitle>
            <DialogDescription>
              1 行目を列名、2 行目以降をデータとして読みます。列の型は値から推定するので、違っていたら直してください。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-1.5">
            <input
              ref={inputRef}
              type="file"
              accept=".csv,text/csv"
              className="sr-only"
              aria-label="テーブルにする CSV ファイル"
              onChange={(e) => {
                void pick(e.target.files?.[0]);
                e.target.value = "";
              }}
            />
            <button
              type="button"
              onClick={() => inputRef.current?.click()}
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                void pick(e.dataTransfer.files?.[0]);
              }}
              className={cn(
                "flex w-full items-center gap-3 rounded-lg border border-dashed px-4 text-left text-sm transition-colors duration-fast hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                plan ? "py-2.5" : "py-6",
              )}
              data-testid="csv-create-drop"
            >
              {reading ? (
                <Loader2 className="size-5 shrink-0 animate-spin text-muted-foreground" aria-hidden />
              ) : (
                <FileUp className="size-5 shrink-0 text-muted-foreground" aria-hidden />
              )}
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{fileName ?? "CSV ファイルを選ぶ"}</span>
                <span className="block text-xs text-muted-foreground">
                  {plan
                    ? `${fmt(plan.columns.length)} 列・${fmt(plan.rows.length)} 行 — 押すと選び直します`
                    : "ここにドロップするか、押して選びます（UTF-8 / Shift_JIS・32 MB まで）"}
                </span>
              </span>
            </button>
            {fileError ? (
              <p role="alert" className="text-xs text-destructive" data-testid="csv-create-error">
                {fileError}
              </p>
            ) : null}
          </div>

          {plan ? (
            <>
              <div className="space-y-1.5">
                <label htmlFor="csv-table-name" className="text-xs font-medium">
                  テーブル名
                </label>
                <Input
                  id="csv-table-name"
                  maxLength={128}
                  value={name}
                  onChange={(e) => {
                    setName(e.target.value);
                    setNameError(null);
                  }}
                  aria-invalid={nameError ? true : undefined}
                  aria-describedby={nameError ? "csv-table-name-error" : undefined}
                />
                {nameError ? (
                  <p id="csv-table-name-error" role="alert" className="text-xs text-destructive">
                    {nameError}
                  </p>
                ) : null}
              </div>
              <div className="space-y-1.5">
                <p className="text-xs font-medium">列</p>
                <div className="max-h-[45vh] overflow-y-auto rounded-lg border" data-testid="csv-create-columns">
                  <table className="w-full text-sm">
                    <thead className="sticky top-0 z-10 bg-muted/80 text-xs text-muted-foreground backdrop-blur">
                      <tr>
                        <th className="px-3 py-1.5 text-left font-medium">列名</th>
                        <th className="w-32 px-3 py-1.5 text-left font-medium">型</th>
                        <th className="px-3 py-1.5 text-left font-medium max-sm:hidden">値の例</th>
                      </tr>
                    </thead>
                    <tbody>
                      {plan.columns.map((c, i) => (
                        <tr key={i} className="border-t" data-testid="csv-create-column">
                          <td className="px-2 py-1.5">
                            <Input
                              className="h-8"
                              maxLength={100}
                              value={c.name}
                              aria-label={`${i + 1} 列目の列名`}
                              aria-invalid={!c.name.trim() ? true : undefined}
                              onChange={(e) => setColumn(i, { name: e.target.value })}
                            />
                          </td>
                          <td className="px-2 py-1.5">
                            <Select value={c.type} onValueChange={(v) => setColumn(i, { type: v as CsvColumnType })}>
                              <SelectTrigger className="h-8" aria-label={`「${c.name || i + 1}」の型`}>
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                {CSV_COLUMN_TYPES.map((t) => (
                                  <SelectItem key={t.value} value={t.value}>
                                    {t.label}
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          </td>
                          <td className="max-w-0 truncate px-3 py-1.5 text-xs text-muted-foreground max-sm:hidden">
                            {c.samples.length ? c.samples.join("、") : "（空）"}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {emptyColumn >= 0 ? (
                  <p role="alert" className="text-xs text-destructive">
                    {emptyColumn + 1} 列目の列名を入れてください。
                  </p>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    型に合わない値の行は取り込まれず、テーブル画面に理由が出ます。
                  </p>
                )}
              </div>
            </>
          ) : null}

          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={() => onOpenChange(false)}>
              キャンセル
            </Button>
            <Button
              type="submit"
              disabled={busy || !plan || !name.trim() || emptyColumn >= 0}
              data-testid="csv-create-submit"
            >
              {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
              作成して取り込む
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
