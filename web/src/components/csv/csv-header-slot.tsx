"use client";

/// CSV 詳細の統一ヘッダ注入（横バー二重の解消）。戻る/名前/件数/閲覧のみ/タブ切替/保存を
/// 共通ヘッダへ寄せる。null を返すだけなのでレイアウトには影響しない。

import { ArrowLeft, Eye, Save, Table2, Terminal } from "lucide-react";
import Link from "next/link";

import { Button } from "@/components/ui/button";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { usePageHeader } from "@/components/shell/page-header-context";

export function CsvHeaderSlot({
  name,
  totalRows,
  cols,
  editable,
  tab,
  onTabChange,
  dirty,
  saving,
  onSave,
}: {
  name: string;
  totalRows: number;
  cols: number;
  editable: boolean;
  tab: "grid" | "sql";
  onTabChange: (t: "grid" | "sql") => void;
  dirty: boolean;
  saving: boolean;
  onSave: () => void;
}) {
  usePageHeader(
    () => (
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <Link
          href="/drive"
          className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-95"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{name}</span>
        <span className="hidden text-xs text-muted-foreground tabular-nums sm:inline">
          {totalRows} 行 × {cols} 列
        </span>
        {!editable ? (
          <span
            className="inline-flex items-center gap-1 rounded-full border bg-muted/50 px-2.5 py-0.5 text-xs font-medium text-muted-foreground"
            data-testid="csv-readonly-badge"
          >
            <Eye className="size-3.5" aria-hidden />
            閲覧のみ
          </span>
        ) : null}
        <SegmentedControl
          aria-label="表示切替"
          value={tab}
          onValueChange={(v) => onTabChange(v as "grid" | "sql")}
          options={[
            { value: "grid", label: "グリッド", icon: Table2, testId: "csv-tab-grid" },
            { value: "sql", label: "SQL", icon: Terminal, testId: "csv-tab-sql" },
          ]}
        />
        {editable ? (
          <Button
            type="button"
            size="sm"
            loading={saving}
            disabled={!dirty || saving}
            onClick={onSave}
            data-testid="csv-save"
          >
            {!saving ? <Save className="size-4" aria-hidden /> : null}
            保存{dirty ? "*" : ""}
          </Button>
        ) : null}
      </div>
    ),
    [name, totalRows, cols, editable, tab, onTabChange, dirty, saving, onSave],
  );
  return null;
}
