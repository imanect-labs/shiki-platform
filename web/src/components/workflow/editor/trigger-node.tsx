"use client";

/// トリガ擬似ノード（schedule / event / manual）。点線スタイルで本体ノードと区別する。
///
/// IR 上トリガはノードではない（triggers[]）。キャンバスでは「きっかけ」として左端に置き、
/// エントリノード（入エッジ 0 本）への破線接続で流れの始まりを可視化する（視覚のみ・IR 不変）。

import * as React from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { AlertCircle, CalendarClock, MousePointerClick, Zap } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { EVENT_CATALOG } from "@/generated/workflow-catalog";
import type { Trigger, ValidationError } from "@/generated/workflow-ir";

export type TriggerNodeData = {
  trigger: Trigger;
  index: number;
  /// 保存時の誤り（選んだフォルダーが見つからないなど）。あれば赤い印を付け、理由はきっかけの設定に出す。
  errors?: ValidationError[];
  [key: string]: unknown;
};

/// きっかけの種類ごとの季節色（schedule=秋/event=夏/manual=春）。差し色で3種を識別する。
export function triggerSeasonIndex(kind: Trigger["kind"]): number {
  return kind === "schedule" ? 2 : kind === "event" ? 1 : 0;
}

export function triggerView(trigger: Trigger): {
  icon: React.ElementType;
  label: string;
  detail: string;
} {
  switch (trigger.kind) {
    case "schedule":
      return {
        icon: CalendarClock,
        label: "スケジュール",
        detail: `${trigger.cron}（${trigger.tz}）`,
      };
    case "event":
      // できごとの名前はイベントのカタログの単一定義から（ファイル保存・テーブルの行の追加／更新／状態の変更）。
      return {
        icon: Zap,
        label: "できごと",
        detail: EVENT_CATALOG.find((e) => e.source === trigger.source)?.label_ja ?? "フォルダへの保存など",
      };
    default:
      return { icon: MousePointerClick, label: "手動で実行", detail: "ボタンから開始" };
  }
}

export function TriggerNode({ data, selected }: NodeProps & { data: TriggerNodeData }) {
  const view = triggerView(data.trigger);
  const Icon = view.icon;
  const accent = seasonVar(triggerSeasonIndex(data.trigger.kind));
  const errorCount = data.errors?.length ?? 0;
  return (
    <div
      className={cn(
        "w-52 rounded-xl border border-dashed bg-background/80 px-3.5 py-2.5 shadow-sm",
        "transition-shadow duration-fast",
        errorCount > 0 && "border-destructive/60",
        selected && "ring-2 ring-primary border-solid",
      )}
      data-testid="trigger-node"
    >
      <div className="flex items-center gap-2.5">
        <span
          className={cn(
            "flex size-7 shrink-0 items-center justify-center rounded-lg",
            errorCount > 0 && "bg-destructive/10 text-destructive",
          )}
          style={
            errorCount > 0
              ? undefined
              : {
                  backgroundColor: "color-mix(in oklab, var(--tk) 16%, transparent)",
                  color: "var(--tk)",
                  ["--tk" as string]: accent,
                }
          }
        >
          <Icon className="size-4" aria-hidden />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            きっかけ
          </span>
          <span className="block truncate text-sm font-medium leading-5">{view.label}</span>
          <span className="block truncate text-[11px] text-muted-foreground">{view.detail}</span>
        </span>
        {errorCount > 0 ? (
          <span
            className="flex size-5 shrink-0 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
            role="img"
            aria-label={`直すところ ${errorCount} 件`}
            title="直すところがあります。押すと理由を確かめられます。"
          >
            <AlertCircle className="size-3.5" aria-hidden />
          </span>
        ) : null}
      </div>
      <Handle
        type="source"
        position={Position.Right}
        isConnectable={false}
        className="!size-2.5 !border-2 !border-background !bg-muted-foreground/60"
      />
    </div>
  );
}
