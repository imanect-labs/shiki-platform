"use client";

/// エッジ（中点にホバーで現れる挿入プラスボタン付き・ポート名ラベル）。

import * as React from "react";
import {
  BaseEdge,
  EdgeLabelRenderer,
  getBezierPath,
  type EdgeProps,
} from "@xyflow/react";
import { Plus } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import type { Edge as IrEdge } from "@/generated/workflow-ir";
import { AddNodeMenu } from "./add-node-menu";

export type PlusEdgeData = {
  irEdge: IrEdge;
  /// このエッジ上に新ノードを割り込ませる。
  onInsert: (nodeType: string, position: { x: number; y: number }) => void;
  errorMessages: string[];
  [key: string]: unknown;
};

const PORT_LABELS: Record<string, string> = {
  true: "はい",
  false: "いいえ",
  error: "エラー時",
  timeout: "時間切れ",
};

export function PlusEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  selected,
  data,
}: EdgeProps & { data: PlusEdgeData }) {
  const [path, labelX, labelY] = getBezierPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });
  const [menuOpen, setMenuOpen] = React.useState(false);
  const hasError = data.errorMessages.length > 0;
  const port = data.irEdge.from_port ?? "out";
  const portText = PORT_LABELS[port] ?? (port !== "out" ? port : "");

  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        className={cn(
          "!stroke-[1.5]",
          hasError
            ? "!stroke-[oklch(0.6_0.15_25)]"
            : selected
              ? "!stroke-primary"
              : "!stroke-border",
        )}
      />
      <EdgeLabelRenderer>
        <div
          style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
          className="nodrag nopan pointer-events-auto absolute"
        >
          <div className="group/edge flex items-center gap-1">
            {portText ? (
              <span className="rounded-full border bg-background px-1.5 py-0.5 text-[10px] leading-none text-muted-foreground">
                {portText}
              </span>
            ) : null}
            <Popover open={menuOpen} onOpenChange={setMenuOpen}>
              <PopoverTrigger asChild>
                <button
                  type="button"
                  aria-label="ここにブロックを挿入"
                  className={cn(
                    "flex size-5 items-center justify-center rounded-full border bg-background text-muted-foreground shadow-sm",
                    "opacity-0 transition-opacity duration-fast group-hover/edge:opacity-100 focus-visible:opacity-100",
                    "hover:border-primary hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    (menuOpen || selected) && "opacity-100",
                  )}
                >
                  <Plus className="size-3.5" aria-hidden />
                </button>
              </PopoverTrigger>
              <PopoverContent side="bottom" align="center" className="w-auto p-3">
                <AddNodeMenu
                  contextLabel="この間にブロックを挿入"
                  onPick={(nodeType) => {
                    setMenuOpen(false);
                    data.onInsert(nodeType, { x: labelX, y: labelY });
                  }}
                />
              </PopoverContent>
            </Popover>
          </div>
        </div>
      </EdgeLabelRenderer>
    </>
  );
}
