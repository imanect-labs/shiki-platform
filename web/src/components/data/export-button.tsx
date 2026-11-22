"use client";

/// テーブルを CSV に書き出すボタン（閲覧者が読める列・行だけ・見出し行つき）。
///
/// スナップショット読取で全ページを辿るので、行が多いと時間がかかる。書き出し中はボタンに行数を出し、
/// もう一度押すと中止する。

import { Download, Loader2 } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { type DataTable } from "@/lib/data-api";
import { exportTableCsv, safeFileName } from "@/lib/data-csv";
import { downloadBlob } from "@/lib/download";

const fmt = (n: number) => n.toLocaleString("ja-JP");

export function ExportButton({ table, masked }: { table: DataTable; masked: ReadonlySet<string> }) {
  const [rows, setRows] = React.useState<number | null>(null);
  const abort = React.useRef<AbortController | null>(null);

  React.useEffect(() => () => abort.current?.abort(), []);

  const run = async () => {
    if (abort.current) {
      abort.current.abort();
      return;
    }
    const ctl = new AbortController();
    abort.current = ctl;
    setRows(0);
    try {
      const r = await exportTableCsv(table, masked, { signal: ctl.signal, onProgress: setRows });
      downloadBlob(r.blob, `${safeFileName(table.name)}.csv`);
      toast({ title: `${fmt(r.rows)} 行を書き出しました` });
    } catch (e) {
      if (ctl.signal.aborted) {
        toast({ title: "書き出しを中止しました" });
      } else {
        toast({
          variant: "destructive",
          title: "書き出せませんでした",
          description: e instanceof Error ? e.message : String(e),
        });
      }
    } finally {
      abort.current = null;
      setRows(null);
    }
  };

  const running = rows != null;
  return (
    <Button
      variant="ghost"
      size="sm"
      className="h-8 gap-1.5 px-2.5 text-[13px]"
      onClick={run}
      data-testid="export-csv"
      data-state={running ? "running" : "idle"}
      aria-label={running ? `CSV に書き出し中（${fmt(rows)} 行）。押すと中止` : "CSV に書き出す"}
      title={running ? "押すと中止します" : "見えている列と行を CSV に書き出します"}
    >
      {running ? (
        <Loader2 className="size-4 animate-spin text-primary" aria-hidden />
      ) : (
        <Download className="size-4" aria-hidden />
      )}
      <span className="max-sm:sr-only">{running ? `書き出し中 ${fmt(rows)} 行` : "書き出す"}</span>
    </Button>
  );
}
