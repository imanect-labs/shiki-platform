"use client";

/// CSV の一括取込（Task 15.11 の API・監査 B3-6）: ファイルを選んで取込を始め、検証 → 適用 → 索引の
/// 進捗と、不合格・拒否された行の理由を見せる。
///
/// 取込はサーバのジョブで進む（ダイアログを閉じても続く）。取込 ID は一覧の API が無いので、テーブルごとに
/// sessionStorage に持ち、開き直したときに同じ取込の進捗を出す。

import { AlertTriangle, CheckCircle2, Download, FileUp, Loader2, Upload } from "lucide-react";
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
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
  DataApiError,
  getImport,
  listImportErrors,
  MAX_IMPORT_BYTES,
  startCsvImport,
  type BulkImportStatus,
  type BulkIndexMode,
  type BulkRowError,
  type DataTable,
} from "@/lib/data-api";
import { isEditable, withFieldLabels } from "@/lib/data-view";
import { cn } from "@/lib/utils";

const POLL_MS = 1_000;
const storageKey = (tableId: string) => `shiki:data-import:${tableId}`;

function readStored(tableId: string): string | null {
  try {
    return window.sessionStorage.getItem(storageKey(tableId));
  } catch {
    return null;
  }
}

function writeStored(tableId: string, id: string | null) {
  try {
    if (id) window.sessionStorage.setItem(storageKey(tableId), id);
    else window.sessionStorage.removeItem(storageKey(tableId));
  } catch {
    // 保存できなくても取込は続く（開き直したときに進捗が出ないだけ）。
  }
}

export function isTerminal(s: BulkImportStatus | null): boolean {
  return s?.status === "done" || s?.status === "failed";
}

/// 取込の進み具合（0〜1・分からない段階は null）。
export function importProgress(s: BulkImportStatus): number | null {
  if (s.status === "done") return 1;
  if (s.status === "applying" && s.rows_valid > 0) return Math.min(1, s.rows_applied / s.rows_valid);
  return null;
}

const PHASES = [
  { key: "validating", label: "検証" },
  { key: "applying", label: "適用" },
  { key: "indexing", label: "索引" },
  { key: "done", label: "完了" },
] as const;

export type ImportJob = {
  status: BulkImportStatus | null;
  /// 取込の開始・進捗の取得の失敗（行エラーではない）。
  error: string | null;
  starting: boolean;
  running: boolean;
  start: (file: File, opts: { indexMode: BulkIndexMode; abortOnInvalid: boolean }) => Promise<void>;
  clear: () => void;
  /// 開始の失敗の文言を消す（ファイルを選び直したとき）。
  dismissError: () => void;
  /// 進捗の取得が一時的に失敗し、自動で取り直している（サーバの再起動中など）。
  reconnecting: boolean;
};

/// テーブルの取込ジョブの状態（開始・進捗のポーリング）。ダイアログを閉じても進捗を追う。
export function useImportJob(tableId: string): ImportJob {
  const [importId, setImportId] = React.useState<string | null>(null);
  const [status, setStatus] = React.useState<BulkImportStatus | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [starting, setStarting] = React.useState(false);
  /// 進捗の取得に失敗した回数（状態が変わらなくてもポーリングを続けるための再登録の契機・間隔を延ばす）。
  const [failures, setFailures] = React.useState(0);

  React.useEffect(() => {
    setImportId(readStored(tableId));
    setStatus(null);
    setError(null);
    setFailures(0);
  }, [tableId]);

  React.useEffect(() => {
    if (!importId || isTerminal(status)) return;
    let active = true;
    const tick = async () => {
      try {
        const s = await getImport(tableId, importId);
        if (active) {
          setStatus(s);
          setError(null);
          setFailures(0);
        }
      } catch (e) {
        if (!active) return;
        if (e instanceof DataApiError && (e.status === 404 || e.status === 403)) {
          // 取込が見つからない（別のテーブル・権限を失った）: 覚えていた ID を捨てる。
          writeStored(tableId, null);
          setImportId(null);
          setStatus(null);
        } else {
          // 一時的な失敗（サーバの再起動中の 503 など）: 止めずに間隔を延ばして取り直す（画面は「再接続中」と出す）。
          setFailures((n) => n + 1);
        }
      }
    };
    const delay = failures ? Math.min(POLL_MS * 2 ** failures, 30_000) : status ? POLL_MS : 0;
    const t = window.setTimeout(tick, delay);
    return () => {
      active = false;
      window.clearTimeout(t);
    };
  }, [importId, status, tableId, failures]);

  const start = React.useCallback(
    async (file: File, opts: { indexMode: BulkIndexMode; abortOnInvalid: boolean }) => {
      setStarting(true);
      setError(null);
      try {
        const r = await startCsvImport(tableId, file, {
          indexMode: opts.indexMode === "auto" ? undefined : opts.indexMode,
          abortOnInvalid: opts.abortOnInvalid,
        });
        writeStored(tableId, r.import_id);
        setImportId(r.import_id);
        setStatus(r.status);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setStarting(false);
      }
    },
    [tableId],
  );

  const clear = React.useCallback(() => {
    writeStored(tableId, null);
    setImportId(null);
    setStatus(null);
    setError(null);
  }, [tableId]);

  const dismissError = React.useCallback(() => setError(null), []);

  return {
    status,
    error,
    starting,
    running: Boolean(importId) && !isTerminal(status),
    start,
    clear,
    dismissError,
    reconnecting: failures > 0 && Boolean(importId) && !isTerminal(status),
  };
}

function formatBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.ceil(n / 1024)} KB`;
  return `${n} B`;
}

const fmt = (n: number) => n.toLocaleString("ja-JP");

/// 見出し行だけの雛形（直接入力できる列の表示名）。
function templateCsv(table: DataTable): string {
  const cols = [...table.schema.fields]
    .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
    .filter((f) => isEditable(f))
    .map((f) => f.display_name || f.key);
  const esc = (s: string) => (/[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  return `${cols.map(esc).join(",")}\r\n`;
}

function PhaseSteps({ status }: { status: BulkImportStatus }) {
  const failed = status.status === "failed";
  const at = PHASES.findIndex((p) => p.key === status.status);
  return (
    <ol className="flex items-center gap-1.5 text-[11px]" aria-label="取込の段階">
      {PHASES.map((p, i) => {
        const done = !failed && (at > i || status.status === "done");
        const current = !failed && at === i && status.status !== "done";
        return (
          <li key={p.key} className="flex items-center gap-1.5">
            {i > 0 ? <span className={cn("h-px w-4 bg-border", done && "bg-primary/60")} aria-hidden /> : null}
            <span
              className={cn(
                "inline-flex items-center gap-1 rounded-full border px-2 py-0.5",
                done && "border-primary/30 bg-primary/10 text-primary",
                current && "border-primary text-foreground",
                !done && !current && "text-muted-foreground",
              )}
              aria-current={current ? "step" : undefined}
            >
              {current ? <Loader2 className="size-3 animate-spin" aria-hidden /> : null}
              {p.label}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

function ProgressBar({ value }: { value: number | null }) {
  return (
    <div
      className="relative h-1.5 w-full overflow-hidden rounded-full bg-muted"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value == null ? undefined : Math.round(value * 100)}
      aria-label="取込の進捗"
    >
      {value == null ? (
        <span className="absolute inset-y-0 left-0 w-1/3 animate-[data-import-indeterminate_1.4s_ease-in-out_infinite] rounded-full bg-primary/70" />
      ) : (
        <span
          className="block h-full rounded-full bg-primary transition-[width] duration-500"
          style={{ width: `${Math.max(2, Math.round(value * 100))}%` }}
        />
      )}
    </div>
  );
}

/// サーバの行エラーの文言は列を参照名（`'amount'`）で書くので、画面では表示名（「金額」）に直す。
/// 取込の誤りに CSV 読取ライブラリ・内部の型の英語が混じる古いサーバの文言は、利用者向けの言い方に置き換える。
function withLabels(message: string, table: DataTable): string {
  return withFieldLabels(friendlyImportMessage(message), table.schema.fields).replace(/」\s+(?=[のはがをに])/g, "」");
}

/// 取込の失敗の文言のうち、内部の英語（CSV ライブラリ・serde の型名）を利用者向けの日本語にする（N10・A P3）。
export function friendlyImportMessage(message: string): string {
  const m = /CSV error: record (\d+) \(line:? ?(\d+)[^)]*\)[^:]*: found record with (\d+) fields?, but the previous record has (\d+) fields?/i.exec(
    message,
  );
  if (m) {
    return `${m[2]} 行目の列の数（${m[3]}）が見出し行（${m[4]}）と合いません。区切りの「,」や引用符「"」を確かめてください`;
  }
  if (/CSV error|invalid utf-?8|stream did not contain valid UTF-8/i.test(message)) {
    return /utf-?8/i.test(message)
      ? "文字コードを読めませんでした。UTF-8 の CSV で保存し直してください"
      : "CSV を読み取れませんでした。区切りの「,」や引用符「\"」、改行を確かめてください";
  }
  if (/expected struct|missing field|unknown field|invalid type/i.test(message)) {
    return "取込の指定の形が正しくありません。画面を読み込み直してからもう一度お試しください";
  }
  return message;
}

function RowErrors({ table, status }: { table: DataTable; status: BulkImportStatus }) {
  const tableId = table.id;
  const [items, setItems] = React.useState<BulkRowError[]>([]);
  const [next, setNext] = React.useState<number | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const total = status.rows_invalid + status.rows_rejected;

  const load = React.useCallback(
    async (after: number | null) => {
      setLoading(true);
      setError(null);
      try {
        const r = await listImportErrors(tableId, status.id, after, 100);
        setItems((xs) => (after == null ? r.items : [...xs, ...r.items]));
        setNext(r.next_after_row ?? null);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [tableId, status.id],
  );

  React.useEffect(() => {
    if (total > 0 && isTerminal(status)) void load(null);
    // 取込が終わってから 1 回だけ読む（途中の件数の変化では読み直さない）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status.id, status.status]);

  if (total === 0 || !isTerminal(status)) return null;
  return (
    <section className="space-y-2" aria-label="取り込めなかった行">
      <h3 className="text-xs font-semibold">
        取り込めなかった行 <span className="font-normal text-muted-foreground">{fmt(total)} 行</span>
      </h3>
      <div className="max-h-56 overflow-y-auto rounded-lg border scrollbar-subtle" data-testid="import-row-errors">
        <table className="w-full text-left text-xs">
          <thead className="sticky top-0 bg-muted/80 text-muted-foreground backdrop-blur">
            <tr>
              <th scope="col" className="w-16 px-2.5 py-1.5 font-medium">行</th>
              <th scope="col" className="px-2.5 py-1.5 font-medium">理由</th>
            </tr>
          </thead>
          <tbody>
            {items.map((e) => (
              <tr key={e.row_no} className="border-t align-top">
                <td className="px-2.5 py-1.5 tabular-nums text-muted-foreground">
                  {e.row_no}
                  {e.key ? <span className="block truncate text-[10px]">{e.key}</span> : null}
                </td>
                <td className="px-2.5 py-1.5 break-words">{withLabels(e.message, table)}</td>
              </tr>
            ))}
            {loading ? (
              <tr className="border-t">
                <td colSpan={2} className="px-2.5 py-2 text-muted-foreground">
                  <Loader2 className="mr-1.5 inline size-3.5 animate-spin" aria-hidden />
                  読み込み中…
                </td>
              </tr>
            ) : null}
          </tbody>
        </table>
      </div>
      {error ? <p className="text-[11px] text-destructive">{error}</p> : null}
      {next != null && !loading ? (
        <Button type="button" variant="ghost" size="sm" className="h-7 text-xs" onClick={() => void load(next)}>
          さらに表示
        </Button>
      ) : null}
      <p className="text-[11px] text-muted-foreground">
        行番号は見出し行を除いたデータの何行目か（1 始まり）です。直した行だけの CSV をもう一度取り込めます。
      </p>
    </section>
  );
}

export function ImportDialog({
  open,
  onOpenChange,
  table,
  job,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  table: DataTable;
  job: ImportJob;
}) {
  const [file, setFile] = React.useState<File | null>(null);
  const [dragging, setDragging] = React.useState(false);
  const [abortOnInvalid, setAbortOnInvalid] = React.useState(false);
  const [indexMode, setIndexMode] = React.useState<BulkIndexMode>("auto");
  const inputRef = React.useRef<HTMLInputElement>(null);
  const status = job.status;
  const tooLarge = file ? file.size > MAX_IMPORT_BYTES : false;

  React.useEffect(() => {
    if (open && !job.running) {
      setFile(null);
      setDragging(false);
    }
    // 開いた瞬間だけ選び直しにする（進行中の取込は進捗を出す）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const pick = (f: File | null | undefined) => {
    if (!f) return;
    setFile(f);
    job.dismissError();
  };

  const downloadTemplate = () => {
    const blob = new Blob(["﻿", templateCsv(table)], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    // 文書に付けてからクリックし、URL の破棄はダウンロードが始まった後にする（同期で破棄するとブラウザが
    // ファイル名を失い `download` になっていた・A-26）。ファイル名に使えない文字は置き換える。
    const a = document.createElement("a");
    a.href = url;
    a.download = `${table.name.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_") || "table"}-雛形.csv`;
    a.style.display = "none";
    document.body.appendChild(a);
    a.click();
    a.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!file || tooLarge || job.starting) return;
    await job.start(file, { indexMode, abortOnInvalid });
  };

  const showProgress = Boolean(status);
  const progress = status ? importProgress(status) : null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg" data-testid="import-dialog">
        <form onSubmit={submit} className="space-y-4" noValidate>
          <DialogHeader>
            <DialogTitle>CSV から取り込む</DialogTitle>
            <DialogDescription>
              {table.name} にレコードを追加します。取込はサーバで進むので、閉じても止まりません。
            </DialogDescription>
          </DialogHeader>

          {showProgress && status ? (
            <div className="space-y-3" data-testid="import-progress" data-status={status.status}>
              <PhaseSteps status={status} />
              {status.status !== "failed" ? <ProgressBar value={progress} /> : null}
              <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-4">
                {[
                  ["行", status.rows_total],
                  ["合格", status.rows_valid],
                  ["適用", status.rows_applied],
                  ["取り込めず", status.rows_invalid + status.rows_rejected],
                ].map(([label, n]) => (
                  <div key={label as string} className="rounded-md bg-muted/50 px-2.5 py-1.5">
                    <dt className="text-[11px] text-muted-foreground">{label}</dt>
                    <dd className="font-medium tabular-nums">{fmt(n as number)}</dd>
                  </div>
                ))}
              </dl>
              {status.status === "done" ? (
                <p className="flex items-start gap-1.5 text-xs" role="status">
                  <CheckCircle2 className="mt-px size-4 shrink-0 text-primary" aria-hidden />
                  {status.rows_applied > 0
                    ? `${fmt(status.rows_applied)} 行を取り込みました。`
                    : "取り込んだ行はありません。"}
                  {status.rows_invalid > 0 && status.rows_applied === 0 && abortOnInvalid
                    ? " 不合格の行があったため、何も適用していません。"
                    : ""}
                </p>
              ) : status.status === "indexing" ? (
                <p className="text-xs text-muted-foreground" role="status">
                  行は取り込み済みです。並べ替え・絞り込み用の索引を仕上げています（その間も表は使えます）。
                </p>
              ) : status.status === "failed" ? (
                <p
                  className="flex items-start gap-1.5 rounded-md border border-destructive/40 bg-destructive/5 px-2.5 py-2 text-xs text-destructive"
                  role="alert"
                >
                  <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
                  取込に失敗しました{status.last_error ? `: ${withLabels(status.last_error, table)}` : "。"}
                </p>
              ) : (
                <p className="text-xs text-muted-foreground" role="status">
                  {status.status === "validating" ? "各行を検証しています…" : "検証を通った行を書き込んでいます…"}
                </p>
              )}
              <RowErrors table={table} status={status} />
            </div>
          ) : (
            <div className="space-y-3">
              <label
                className={cn(
                  "flex cursor-pointer flex-col items-center justify-center gap-1.5 rounded-xl border border-dashed px-4 py-7 text-center text-sm transition-colors",
                  "has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring",
                  dragging ? "border-primary bg-primary/5" : "hover:bg-accent/50",
                )}
                onDragOver={(e) => {
                  e.preventDefault();
                  setDragging(true);
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDragging(false);
                  pick(e.dataTransfer.files?.[0]);
                }}
                data-testid="import-drop"
              >
                <input
                  ref={inputRef}
                  type="file"
                  accept=".csv,text/csv"
                  className="sr-only"
                  aria-label="取り込む CSV ファイル"
                  onChange={(e) => pick(e.target.files?.[0])}
                />
                {file ? (
                  <>
                    <FileUp className="size-6 text-primary" aria-hidden />
                    <span className="max-w-full truncate font-medium">{file.name}</span>
                    <span className={cn("text-xs", tooLarge ? "text-destructive" : "text-muted-foreground")}>
                      {formatBytes(file.size)}
                      {tooLarge ? `・上限 ${formatBytes(MAX_IMPORT_BYTES)} を超えています。分割してください` : "・別のファイルを選ぶ"}
                    </span>
                  </>
                ) : (
                  <>
                    <Upload className="size-6 text-muted-foreground" aria-hidden />
                    <span className="font-medium">CSV ファイルを選ぶ</span>
                    <span className="text-xs text-muted-foreground">またはここにドロップ（UTF-8・最大 32 MB）</span>
                  </>
                )}
              </label>
              <ul className="list-disc space-y-0.5 pl-4 text-[11px] leading-relaxed text-muted-foreground">
                <li>1 行目は列名（表示名か参照名）。各行は新しいレコードとして追加します。</li>
                <li>空欄は値なし。複数選択・リンクは <code>a;b</code> か JSON の配列で書きます。</li>
                <li><code>_key</code> 列（任意・取込内で一意）を置くと、取り込めなかった行をその値でも示します。</li>
              </ul>
              <button
                type="button"
                onClick={downloadTemplate}
                className="inline-flex items-center gap-1 rounded text-xs font-medium text-primary hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Download className="size-3.5" aria-hidden />
                見出し行の雛形をダウンロード
              </button>
              <div className="space-y-3 rounded-lg border px-3 py-3">
                <div className="flex items-center justify-between gap-3">
                  <label htmlFor="import-abort" className="text-xs">
                    <span className="font-medium">不合格の行があれば何も取り込まない</span>
                    <span className="block text-[11px] text-muted-foreground">オフなら合格した行だけ取り込みます</span>
                  </label>
                  <Switch id="import-abort" checked={abortOnInvalid} onCheckedChange={setAbortOnInvalid} />
                </div>
                <div className="flex items-center justify-between gap-3">
                  <label htmlFor="import-index-mode" className="text-xs">
                    <span className="font-medium">索引の作り方</span>
                    <span className="block text-[11px] text-muted-foreground">大量の行は「後で作る」が速く終わります</span>
                  </label>
                  <Select value={indexMode} onValueChange={(v) => setIndexMode(v as BulkIndexMode)}>
                    <SelectTrigger id="import-index-mode" className="h-8 w-32 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="auto">自動</SelectItem>
                      <SelectItem value="immediate">すぐ作る</SelectItem>
                      <SelectItem value="deferred">後で作る</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>
            </div>
          )}

          {job.reconnecting ? (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground" role="status" data-testid="import-reconnecting">
              <Loader2 className="size-3.5 shrink-0 animate-spin" aria-hidden />
              サーバーに接続できません。自動で再接続しています…（取込はサーバー側で続いています）
            </p>
          ) : null}
          {job.error ? (
            <p
              className="flex items-start gap-1.5 rounded-md border border-destructive/40 bg-destructive/5 px-2.5 py-2 text-xs text-destructive"
              role="alert"
              data-testid="import-error"
            >
              <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
              <span className="min-w-0 break-words">{withLabels(job.error, table)}</span>
            </p>
          ) : null}

          <DialogFooter className="gap-2">
            {showProgress ? (
              <>
                {isTerminal(status) ? (
                  <Button type="button" variant="outline" onClick={job.clear} data-testid="import-again">
                    <Upload className="size-4" aria-hidden />
                    別のファイルを取り込む
                  </Button>
                ) : null}
                <Button type="button" onClick={() => onOpenChange(false)} data-testid="import-close">
                  {isTerminal(status) ? "閉じる" : "閉じる（取込は続きます）"}
                </Button>
              </>
            ) : (
              <>
                <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
                  キャンセル
                </Button>
                <Button
                  type="submit"
                  disabled={!file || tooLarge || job.starting}
                  data-testid="import-submit"
                >
                  {job.starting ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                  取り込む
                </Button>
              </>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
