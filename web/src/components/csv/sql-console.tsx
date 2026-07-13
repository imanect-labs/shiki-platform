"use client";

/// CSV の読み取り専用 SQL コンソール（Task 11P.8）。
///
/// テーブル名は `data`。結果は簡易テーブルで表示し、「新規 CSV として保存」を明示操作で提供。
/// 実行は tabular サービス（11P.7・隔離 DuckDB）経由の RO SQL。

import { Play, Save } from "lucide-react";
import * as React from "react";

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
import { runQuery, saveNewCsv, tableToCsv, type TableResponse } from "@/lib/tabular-api";

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
    setRunning(true);
    setError(null);
    const started = performance.now();
    try {
      const res = await runQuery(nodeId, sql);
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
    <div className="flex h-full min-h-0 flex-col gap-2 p-3" data-testid="sql-console">
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
          aria-label="SQL"
          data-testid="sql-input"
          className="scrollbar-subtle min-h-16 flex-1 resize-y rounded-md border bg-background p-2 font-mono text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="flex flex-col gap-2">
          <Button
            type="button"
            size="sm"
            loading={running}
            onClick={() => void run()}
            disabled={running}
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
          <kbd className="rounded border bg-muted/50 px-1.5 py-0.5 text-center text-[10px] font-medium text-muted-foreground">
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
        <div className="scrollbar-subtle min-h-0 flex-1 overflow-auto rounded-lg border shadow-xs">
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
