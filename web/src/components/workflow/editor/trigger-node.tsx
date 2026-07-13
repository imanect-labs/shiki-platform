"use client";

/// トリガ擬似ノード（schedule / event / manual）。点線スタイルで本体ノードと区別する。
///
/// IR 上トリガはノードではない（triggers[]）。キャンバスでは「きっかけ」として左端に置き、
/// エントリノード（入エッジ 0 本）への破線接続で流れの始まりを可視化する（視覚のみ・IR 不変）。

import * as React from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { CalendarClock, MousePointerClick, Zap } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import type { Trigger } from "@/generated/workflow-ir";

export type TriggerNodeData = {
  trigger: Trigger;
  index: number;
  [key: string]: unknown;
};

/// きっかけの種類ごとの季節色（schedule=秋/event=夏/manual=春）。差し色で3種を識別する。
function triggerSeasonIndex(kind: Trigger["kind"]): number {
  return kind === "schedule" ? 2 : kind === "event" ? 1 : 0;
}

function triggerView(trigger: Trigger): {
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
      return { icon: Zap, label: "できごと", detail: "フォルダへの保存など" };
    default:
      return { icon: MousePointerClick, label: "手動で実行", detail: "ボタンから開始" };
  }
}

export function TriggerNode({ data, selected }: NodeProps & { data: TriggerNodeData }) {
  const view = triggerView(data.trigger);
  const Icon = view.icon;
  const accent = seasonVar(triggerSeasonIndex(data.trigger.kind));
  return (
    <div
      className={cn(
        "w-52 rounded-xl border border-dashed bg-background/80 px-3.5 py-2.5 shadow-sm",
        "transition-shadow duration-fast",
        selected && "ring-2 ring-primary border-solid",
      )}
    >
      <div className="flex items-center gap-2.5">
        <span
          className="flex size-7 shrink-0 items-center justify-center rounded-lg"
          style={{
            backgroundColor: "color-mix(in oklab, var(--tk) 16%, transparent)",
            color: "var(--tk)",
            ["--tk" as string]: accent,
          }}
        >
          <Icon className="size-4" aria-hidden />
        </span>
        <span className="min-w-0">
          <span className="block text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            きっかけ
          </span>
          <span className="block truncate text-sm font-medium leading-5">{view.label}</span>
          <span className="block truncate text-[11px] text-muted-foreground">{view.detail}</span>
        </span>
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
