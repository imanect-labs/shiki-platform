"use client";

/// generative UI の表示専用テーブル（Task 6.6・データは props 内・Phase 9 の構造化データとは別物）。

import type { CellValue, TableProps } from "@/generated/gui-spec";
import { cn } from "@/lib/utils";

function renderCell(cell: CellValue): string {
  if (typeof cell === "boolean") return cell ? "はい" : "いいえ";
  if (typeof cell === "number") return new Intl.NumberFormat("ja-JP").format(cell);
  return String(cell);
}

export function GenUiTable({ table }: { table: TableProps }) {
  const columns = table.columns ?? [];
  const rows = table.rows ?? [];
  return (
    <figure className="min-w-0" data-testid="genui-table">
      {table.title ? (
        <figcaption className="mb-2 text-[13px] font-semibold tracking-wide text-foreground/80">
          {table.title}
        </figcaption>
      ) : null}
      <div className="overflow-x-auto rounded-lg border border-border">
        <table className="w-full border-collapse text-sm">
          <thead>
            <tr className="border-b border-border bg-secondary/60">
              {columns.map((col, i) => (
                <th
                  key={i}
                  scope="col"
                  className={cn(
                    "px-3 py-2 text-xs font-semibold text-foreground/70",
                    col.align === "right"
                      ? "text-right"
                      : col.align === "center"
                        ? "text-center"
                        : "text-left",
                  )}
                >
                  {col.label}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, ri) => (
              <tr key={ri} className="border-b border-border/60 last:border-b-0 hover:bg-secondary/30">
                {columns.map((col, ci) => (
                  <td
                    key={ci}
                    className={cn(
                      "px-3 py-2 text-foreground/90",
                      typeof row[ci] === "number" || col.align === "right"
                        ? "text-right tabular-nums"
                        : col.align === "center"
                          ? "text-center"
                          : "text-left",
                    )}
                  >
                    {ci < row.length ? renderCell(row[ci]) : ""}
                  </td>
                ))}
              </tr>
            ))}
            {rows.length === 0 ? (
              <tr>
                <td colSpan={Math.max(columns.length, 1)} className="px-3 py-4 text-center text-xs text-muted-foreground">
                  データがありません
                </td>
              </tr>
            ) : null}
          </tbody>
        </table>
      </div>
    </figure>
  );
}
