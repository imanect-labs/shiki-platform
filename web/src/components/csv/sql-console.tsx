"use client";

/// CSV の読み取り専用 SQL コンソール（Task 11P.8）。
///
/// テーブル名は `data`。「CSV を追加」でドライブの別 CSV を名前付きテーブルとして足すと JOIN できる
/// （#1037・追加した CSV にも閲覧権限が要る）。結果は簡易テーブルで表示し、「新規 CSV として保存」を
/// 明示操作で提供。実行は tabular サービス（11P.7・隔離 DuckDB）経由の RO SQL。

import { Play, Plus, Save, X } from "lucide-react";
import * as React from "react";

import { DrivePicker } from "@/components/chat/drive-picker";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/use-toast";
import {
  deriveTableName,
  isValidTableName,
  MAX_EXTRA_TABLES,
  PRIMARY_TABLE,
} from "@/lib/csv/table-name";
import type { NodeResponse } from "@/lib/storage";
import { runQuery, saveNewCsv, tableToCsv, type TableResponse } from "@/lib/tabular-api";
import { cn } from "@/lib/utils";

/// JOIN 用に追加した CSV（テーブル名は利用者が変えられる）。
type ExtraTable = { key: string; name: string; fileId: string; fileName: string };

const isCsvNode = (n: NodeResponse) =>
  n.name.toLowerCase().endsWith(".csv") || (n.content_type ?? "").startsWith("text/csv");

export function SqlConsole({
  nodeId,
  parentId,
  onSaved,
}: {
  nodeId: string;
  parentId?: string | null;
  onSaved?: (savedNodeId: string) => void;
}) {
  const [sql, setSql] = React.useState("SELECT * FROM data LIMIT 100");
  const [result, setResult] = React.useState<TableResponse | null>(null);
  const [running, setRunning] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = React.useState<number | null>(null);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saveName, setSaveName] = React.useState("query-result");
  const [savingCsv, setSavingCsv] = React.useState(false);
  const [tables, setTables] = React.useState<ExtraTable[]>([]);
  const [pickerOpen, setPickerOpen] = React.useState(false);

  // テーブル名の検証（形・data との衝突・重複）。最終判定は backend だが、送る前に気づけるようにする。
  const tableErrors = React.useMemo(() => {
    const errors = new Map<string, string>();
    const seen = new Set<string>();
    for (const t of tables) {
      if (t.name === PRIMARY_TABLE) errors.set(t.key, "data はこの CSV の名前です");
      else if (!isValidTableName(t.name))
        errors.set(t.key, "英小文字・数字・_ で、先頭は数字以外にしてください");
      else if (seen.has(t.name)) errors.set(t.key, "同じ名前が使われています");
      seen.add(t.name);
    }
    return errors;
  }, [tables]);
  const firstTableError = tableErrors.values().next().value as string | undefined;

  const addTable = (node: NodeResponse) => {
    setTables((cur) => [
      ...cur,
      {
        key: `${node.id}-${Date.now()}`,
        name: deriveTableName(
          node.name,
          cur.map((t) => t.name),
        ),
        fileId: node.id,
        fileName: node.name,
      },
    ]);
  };

  // 数値列（右寄せ表示・表計算ソフトの体裁）。結果のサンプルから判定する。
  const numericCols = React.useMemo(() => {
    const nums = new Set<number>();
    if (!result) return nums;
    for (let c = 0; c < result.columns.length; c++) {
      let sawValue = false;
      let allNumeric = true;
      for (const row of result.rows) {
        const v = row[c];
        if (v == null || v === "") continue;
        sawValue = true;
        if (!Number.isFinite(Number(v.replace(/,/g, "")))) {
          allNumeric = false;
          break;
        }
      }
      if (sawValue && allNumeric) nums.add(c);
    }
    return nums;
  }, [result]);

  const run = async () => {
    if (tableErrors.size > 0) {
      setError(`テーブル名を直してください: ${firstTableError}`);
      return;
    }
    setRunning(true);
    setError(null);
    const started = performance.now();
    try {
      const res = await runQuery(
        nodeId,
        sql,
        tables.map((t) => ({ name: t.name, file_id: t.fileId })),
      );
      setResult(res);
      setElapsedMs(Math.round(performance.now() - started));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setResult(null);
      setElapsedMs(null);
    } finally {
      setRunning(false);
    }
  };

  const saveResult = async () => {
    // savingCsv 中は二重発火を防ぐ（Enter と 保存ボタンの多重呼び出し対策）。
    if (!result || !saveName.trim() || savingCsv) return;
    setSavingCsv(true);
    try {
      const saved = await saveNewCsv({ parentId, name: saveName.trim(), csv: tableToCsv(result) });
      toast({ title: "保存しました", description: `${saved.name} を作成しました。` });
      setSaveOpen(false);
      onSaved?.(saved.node_id);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "保存に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setSavingCsv(false);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-2 p-3 max-md:px-1" data-testid="sql-console">
      {/* テーブル一覧: この CSV は data 固定。追加した CSV は名前を付けて JOIN に使う。 */}
      <div className="flex flex-wrap items-center gap-1.5" data-testid="sql-tables">
        <span className="text-xs font-medium text-muted-foreground">テーブル</span>
        <span
          className="inline-flex h-8 items-center gap-1.5 rounded-md border border-border/60 bg-card/40 px-2 text-xs md:h-7"
          title="この CSV"
        >
          <code className="font-mono text-base font-semibold md:text-xs">{PRIMARY_TABLE}</code>
          <span className="text-muted-foreground">この CSV</span>
        </span>
        {tables.map((t) => {
          const err = tableErrors.get(t.key);
          return (
            <span
              key={t.key}
              className={cn(
                "inline-flex h-8 max-w-full items-center gap-1.5 rounded-md border border-border/60 bg-card/40 pr-0.5 pl-1 text-xs md:h-7",
                err && "border-destructive/60",
              )}
              data-testid="sql-table"
            >
              <input
                value={t.name}
                onChange={(e) => {
                  const name = e.target.value;
                  setTables((cur) => cur.map((x) => (x.key === t.key ? { ...x, name } : x)));
                }}
                aria-label={`${t.fileName} のテーブル名`}
                aria-invalid={err ? true : undefined}
                title={err}
                spellCheck={false}
                autoCapitalize="none"
                autoCorrect="off"
                data-testid="sql-table-name"
                // 名前の長さに合わせて伸縮（短い名前の後ろに空白を作らない・長い名前は 20ch で頭打ち）。
                style={{ width: `${Math.min(Math.max(t.name.length, 4), 20) + 1}ch` }}
                className="h-6 min-w-0 rounded-sm bg-transparent px-1 font-mono text-base font-semibold transition-colors hover:bg-accent/60 focus-visible:bg-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring/40 focus-visible:ring-offset-0 md:h-5 md:text-xs"
              />
              <span className="max-w-40 truncate text-muted-foreground" title={t.fileName}>
                {t.fileName}
              </span>
              <button
                type="button"
                onClick={() => setTables((cur) => cur.filter((x) => x.key !== t.key))}
                aria-label={`${t.name} を外す`}
                className="grid size-6 place-items-center rounded-sm text-muted-foreground hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-3.5" aria-hidden />
              </button>
            </span>
          );
        })}
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={() => setPickerOpen(true)}
          disabled={tables.length >= MAX_EXTRA_TABLES}
          title={
            tables.length >= MAX_EXTRA_TABLES
              ? `追加できるのは ${MAX_EXTRA_TABLES} 個までです`
              : "別の CSV をテーブルとして追加し、JOIN できるようにします"
          }
          data-testid="sql-add-table"
        >
          <Plus className="size-3.5" aria-hidden />
          CSV を追加
        </Button>
      </div>
      {firstTableError ? (
        <p className="-mt-1 text-xs text-destructive" data-testid="sql-table-error">
          テーブル名: {firstTableError}
        </p>
      ) : tables.length > 0 ? (
        <p className="-mt-1 text-xs text-muted-foreground">
          例: <code className="font-mono">SELECT * FROM data JOIN {tables[0].name} ON …</code>
        </p>
      ) : null}
      <div className="flex items-start gap-2">
        <textarea
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault();
              void run();
            }
          }}
          rows={3}
          spellCheck={false}
          autoCapitalize="none"
          autoCorrect="off"
          aria-label="SQL"
          data-testid="sql-input"
          className="scrollbar-subtle min-h-16 min-w-0 flex-1 resize-y rounded-md border bg-background p-2 font-mono text-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:text-sm"
        />
        <div className="flex flex-col gap-2">
          <Button
            type="button"
            size="sm"
            loading={running}
            onClick={() => void run()}
            disabled={running || tableErrors.size > 0}
            data-testid="sql-run"
          >
            {!running ? <Play className="size-4" aria-hidden /> : null}
            実行
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => setSaveOpen(true)}
            disabled={!result}
            data-testid="sql-save"
          >
            <Save className="size-4" aria-hidden />
            新規 CSV
          </Button>
          {/* キーボードショートカットの案内はマウスの端末だけ（タッチでは押せない）。 */}
          <kbd className="rounded border bg-muted/50 px-1.5 py-0.5 text-center text-[10px] font-medium text-muted-foreground pointer-coarse:hidden">
            ⌘↵ で実行
          </kbd>
        </div>
      </div>
      {error && (
        <p
          className="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
          data-testid="sql-error"
        >
          {error}
        </p>
      )}
      {result && (
        <div className="flex items-center gap-2 px-0.5 text-xs text-muted-foreground">
          <span className="font-medium text-foreground/70">{result.rows.length} 行</span>
          {elapsedMs != null ? <span>· {elapsedMs} ms</span> : null}
          {result.truncated ? (
            <span className="text-[color:var(--season-autumn)]">· 上限で打ち切り</span>
          ) : null}
        </div>
      )}
      {result && (
        <div className="scrollbar-subtle min-h-0 flex-1 overflow-auto overscroll-contain rounded-lg border shadow-xs">
          <table className="w-full border-collapse text-sm">
            <thead className="sticky top-0 bg-muted/60">
              <tr>
                {result.columns.map((c, j) => (
                  <th
                    key={c}
                    className={`border-b px-3 py-1.5 font-semibold ${numericCols.has(j) ? "text-right" : "text-left"}`}
                  >
                    {c}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {result.rows.map((row, i) => (
                <tr key={i} className="odd:bg-muted/20">
                  {row.map((cell, j) => (
                    <td
                      key={j}
                      className={`border-b px-3 py-1 tabular-nums ${numericCols.has(j) ? "text-right" : "text-left"}`}
                    >
                      {cell ?? ""}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
          {result.truncated && (
            <p className="px-3 py-2 text-xs text-muted-foreground">
              結果が上限で打ち切られました（全件は絞り込んでください）。
            </p>
          )}
        </div>
      )}

      <DrivePicker
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        onSelect={addTable}
        title="JOIN する CSV を選択"
        description="選んだ CSV は名前を付けたテーブルとして SQL から使えます。"
        accept={isCsvNode}
      />

      {/* 新規 CSV として保存（window.prompt の代わりにアプリの Dialog を使う） */}
      <Dialog open={saveOpen} onOpenChange={setSaveOpen}>
        <DialogContent data-testid="sql-save-dialog">
          <DialogHeader>
            <DialogTitle>新規 CSV として保存</DialogTitle>
          </DialogHeader>
          <div className="flex flex-col gap-1.5">
            <label htmlFor="sql-save-name" className="text-sm font-medium">
              ファイル名
            </label>
            <Input
              id="sql-save-name"
              value={saveName}
              onChange={(e) => setSaveName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void saveResult();
                }
              }}
              autoFocus
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              enterKeyHint="done"
              placeholder="query-result"
            />
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setSaveOpen(false)}>
              キャンセル
            </Button>
            <Button loading={savingCsv} disabled={!saveName.trim()} onClick={() => void saveResult()}>
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
