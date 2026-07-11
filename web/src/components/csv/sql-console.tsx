"use client";

/// CSV の読み取り専用 SQL コンソール（Task 11P.8）。
///
/// テーブル名は `data`。結果は簡易テーブルで表示し、「新規 CSV として保存」を明示操作で提供。
/// 実行は tabular サービス（11P.7・隔離 DuckDB）経由の RO SQL。

import { Loader2, Play, Save } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
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
    try {
      setResult(await runQuery(nodeId, sql));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setResult(null);
    } finally {
      setRunning(false);
    }
  };

  const saveResult = async () => {
    if (!result) return;
    const name = window.prompt("新しい CSV の名前", "query-result");
    if (!name) return;
    try {
      const saved = await saveNewCsv({ parentId, name, csv: tableToCsv(result) });
      toast({ title: "保存しました", description: `${saved.name} を作成しました。` });
      onSaved?.(saved.node_id);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "保存に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
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
          className="min-h-16 flex-1 resize-y rounded-md border bg-background p-2 font-mono text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="flex flex-col gap-2">
          <Button type="button" size="sm" onClick={() => void run()} disabled={running} data-testid="sql-run">
            {running ? <Loader2 className="mr-1 size-4 animate-spin" /> : <Play className="mr-1 size-4" />}
            実行
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => void saveResult()}
            disabled={!result}
            data-testid="sql-save"
          >
            <Save className="mr-1 size-4" />
            新規 CSV
          </Button>
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
        <div className="min-h-0 flex-1 overflow-auto rounded-md border">
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
    </div>
  );
}
