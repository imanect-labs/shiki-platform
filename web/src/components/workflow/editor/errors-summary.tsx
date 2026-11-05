"use client";

/// 保存を止める誤りのうち、ブロックにも線にも紐付かないもの（きっかけ・入力項目・ワークフロー全体）の一覧（GAP-35）。
///
/// きっかけのテーブル・フォルダーが見つからない、入力項目の形が確かめられない、などはブロックの赤い印に出ない。
/// ここに場所と理由を並べ、きっかけ・入力項目の誤りは押すとその設定を開く（欄の下にも同じ理由が出る）。入力項目は
/// きっかけと別の入力項目の設定で開く（「手動で実行」のきっかけが無いワークフローでも開ける）。

import * as React from "react";
import { AlertCircle } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import type { Trigger, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import type { Selection } from "./ir-state";
import { triggerView } from "./trigger-node";
import { mapErrors, withoutTriggerPrefix } from "./validation";

/// 誤り 1 件の場所の呼び名と、押したときに開く設定（開けないものは null）。
function placeOf(
  e: ValidationError,
  ir: WorkflowIr,
  triggerIndex: number | null,
): { label: string; select: Selection | null } {
  const triggers = ir.triggers as Trigger[];
  if (triggerIndex !== null) {
    const t = triggers[triggerIndex];
    const label = t ? `きっかけ（${triggerView(t).label}）` : "きっかけ";
    return { label, select: t ? { kind: "trigger", index: triggerIndex } : null };
  }
  if (e.path === "/input_schema" || e.path?.startsWith("/input_schema/")) {
    // 入力項目は、きっかけと別に開く入力項目の設定で直す（「手動で実行」のきっかけが無くても開ける）。
    return { label: "入力項目", select: { kind: "inputs" } };
  }
  return { label: "ワークフロー全体", select: null };
}

export function ErrorsSummary({
  errors,
  ir,
  onSelect,
}: {
  errors: ValidationError[];
  ir: WorkflowIr;
  onSelect: (selection: Selection) => void;
}) {
  const [open, setOpen] = React.useState(false);
  const items = React.useMemo(() => {
    const map = mapErrors(errors);
    const triggerOf = new Map<ValidationError, number>();
    for (const [index, list] of map.byTrigger) for (const e of list) triggerOf.set(e, index);
    return map.global.map((e) => ({ error: e, ...placeOf(e, ir, triggerOf.get(e) ?? null) }));
  }, [errors, ir]);
  if (items.length === 0) return null;
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          data-testid="workflow-errors"
          className="inline-flex items-center gap-1.5 rounded-full border border-destructive/50 bg-background/95 px-3 py-1.5 text-xs font-medium text-destructive shadow-sm outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <AlertCircle className="size-3.5" aria-hidden />
          直すところ {items.length} 件
        </button>
      </PopoverTrigger>
      <PopoverContent align="center" className="w-[min(26rem,calc(100vw-2rem))] p-0">
        <div className="border-b px-3 py-2.5">
          <p className="text-sm font-medium">直すまで保存できません</p>
          <p className="mt-0.5 text-xs text-muted-foreground">ブロックの外の設定に、直すところがあります。</p>
        </div>
        <ul className="max-h-72 divide-y overflow-y-auto scrollbar-subtle" data-testid="workflow-errors-list">
          {items.map((item, i) => (
            <li key={i}>
              <button
                type="button"
                disabled={!item.select}
                onClick={() => {
                  if (!item.select) return;
                  onSelect(item.select);
                  setOpen(false);
                }}
                className="w-full px-3 py-2 text-left text-xs outline-none transition-colors enabled:hover:bg-accent focus-visible:bg-accent"
              >
                <span className="block font-medium">{item.label}</span>
                <span className="mt-0.5 block leading-relaxed text-muted-foreground [overflow-wrap:anywhere]">
                  {withoutTriggerPrefix(item.error.message)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      </PopoverContent>
    </Popover>
  );
}
