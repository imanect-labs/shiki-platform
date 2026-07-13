"use client";

/// キャンバス上のノードカード（アイコン＋日本語ラベル＋ポート＋末尾プラスボタン＋エラーバッジ）。
///
/// - 出力ポートはカタログ＋設定から導出（branch=true/false・switch=cases・wait の timeout・
///   on_error=continue の error）。ポートが複数のときは右辺に縦に並べ、名前を添える。
/// - **末尾のプラスボタン**が主導線: クリックで AddNodeMenu を開き、選んだブロックを
///   このノードの既定ポートへつないで追加する（ノードの「尻尾」に生える）。

import * as React from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { AlertCircle, Plus } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { Node as IrNode, ValidationError } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import { AddNodeMenu } from "./add-node-menu";
import { nodeIcon } from "./icons";
import { categoryVar } from "../category-accent";

export type NodeCardData = {
  irNode: IrNode;
  errors: ValidationError[];
  /// このノードの `port` から新ノードをつないで追加する。
  onAddFrom: (port: string, nodeType: string) => void;
  [key: string]: unknown;
};

/// ノードの出力ポート（カタログ＋設定から導出・ir.md §5.2）。
export function outputPorts(irNode: IrNode): string[] {
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  let ports: string[];
  if (irNode.type === "control.switch") {
    const cases = (irNode.params as { cases?: { port: string }[] } | null)?.cases ?? [];
    // どの case にも一致しない値は実行エンジンが `default` へ流す（control/mod.rs）。
    // フォールバック経路を配線できるよう常に default ハンドルを出す（`out` は発しない）。
    ports = [...new Set([...cases.map((c) => c.port), "default"])];
  } else {
    ports = [...(entry?.output_ports ?? ["out"])];
  }
  if (
    irNode.type === "control.wait" &&
    (irNode.params as { on_timeout?: string } | null)?.on_timeout === "continue"
  ) {
    ports.push("timeout");
  }
  if (irNode.on_error === "continue") ports.push("error");
  return ports;
}

const PORT_LABELS: Record<string, string> = {
  out: "",
  true: "はい",
  false: "いいえ",
  default: "その他",
  error: "エラー時",
  timeout: "時間切れ",
};

function portLabel(port: string): string {
  return PORT_LABELS[port] ?? port;
}

function portColor(port: string): string {
  if (port === "error") return "!bg-destructive";
  if (port === "timeout") return "!bg-[var(--season-autumn)]";
  return "!bg-primary";
}

export function NodeCard({ data, selected }: NodeProps & { data: NodeCardData }) {
  const { irNode, errors, onAddFrom } = data;
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  const Icon = nodeIcon(irNode.type);
  const ports = outputPorts(irNode);
  const hasError = errors.length > 0;
  const [menuOpen, setMenuOpen] = React.useState(false);
  // カテゴリ由来の季節色（アイコンチップの tint で示す）。エラー時は破壊的色を優先。
  const accent = categoryVar(entry?.category);

  return (
    <div
      style={{ ["--accent" as string]: accent }}
      // ⚠️ overflow-hidden は付けない（尻尾の＋が -right-9 でカード外に出るため一緒に切れる）。
      className={cn(
        "group/node relative w-60 rounded-xl border bg-card text-card-foreground shadow-sm",
        // ⚠️ xyflow ノードは motion layout を使わず CSS transition のみ（transform/shadow/border）。
        "transition-[transform,box-shadow,border-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        "hover:-translate-y-0.5 hover:shadow-md",
        selected && "shadow-lg ring-2 ring-primary",
        hasError && "border-destructive",
      )}
    >
      {/* 入力ポート（join は複数エッジを受けるが handle は 1 つ）。 */}
      <Handle
        type="target"
        position={Position.Left}
        className="!size-2.5 !border-2 !border-background !bg-muted-foreground transition-transform duration-[var(--duration-fast)] hover:!scale-125"
      />

      <div className="flex items-start gap-2.5 px-3.5 py-3">
        <span
          className={cn(
            "mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg",
            hasError ? "bg-destructive/10 text-destructive" : "",
          )}
          style={
            hasError
              ? undefined
              : {
                  backgroundColor: "color-mix(in oklab, var(--accent) 14%, transparent)",
                  color: "var(--accent)",
                }
          }
        >
          <Icon className="size-4" aria-hidden />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium leading-5">
            {irNode.label || entry?.label_ja || irNode.type}
          </span>
          <span className="block truncate text-xs text-muted-foreground">
            {entry?.description_ja ?? irNode.type}
          </span>
        </span>
        {hasError ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full bg-destructive text-destructive-foreground"
                aria-label={`検証エラー ${errors.length} 件`}
              >
                <AlertCircle className="size-3.5" aria-hidden />
              </span>
            </TooltipTrigger>
            <TooltipContent side="top" className="max-w-72">
              <ul className="list-disc space-y-1 pl-4 text-xs">
                {errors.slice(0, 5).map((e, i) => (
                  <li key={i}>{e.message}</li>
                ))}
                {errors.length > 5 ? <li>ほか {errors.length - 5} 件</li> : null}
              </ul>
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>

      {/* 出力ポート（複数のときはラベル付きで下部に列挙）。 */}
      {ports.length === 1 ? (
        <Handle
          id={ports[0]}
          type="source"
          position={Position.Right}
          className={cn(
            "!size-2.5 !border-2 !border-background transition-transform duration-[var(--duration-fast)] hover:!scale-125",
            portColor(ports[0]),
          )}
        />
      ) : (
        <div className="border-t px-3.5 py-1.5">
          {ports.map((port, i) => (
            <div key={port} className="relative flex h-6 items-center justify-end">
              <span className="text-[11px] text-muted-foreground">{portLabel(port) || port}</span>
              <Handle
                id={port}
                type="source"
                position={Position.Right}
                style={{ top: `${i * 24 + 12}px` }}
                className={cn(
                  "!absolute !-right-[13px] !size-2.5 !border-2 !border-background",
                  portColor(port),
                )}
              />
            </div>
          ))}
        </div>
      )}

      {/* 尻尾のプラスボタン（主導線・ノードの右側に生える）。 */}
      <Popover open={menuOpen} onOpenChange={setMenuOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label="次のブロックを追加"
            className={cn(
              "nodrag absolute -right-9 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center",
              "rounded-full border bg-background text-muted-foreground shadow-sm",
              // 常時うっすら表示し（発見性）、ホバー/フォーカスで全表示＋プライマリ強調。
              "opacity-60 transition-all duration-fast group-hover/node:opacity-100 focus-visible:opacity-100",
              "hover:border-primary hover:bg-primary hover:text-primary-foreground hover:scale-110",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              menuOpen && "opacity-100 border-primary bg-primary text-primary-foreground",
            )}
          >
            <Plus className="size-4" aria-hidden />
          </button>
        </PopoverTrigger>
        <PopoverContent side="right" align="start" className="w-auto p-3">
          <AddNodeMenu
            contextLabel={`「${irNode.label || entry?.label_ja || irNode.id}」の後ろに追加`}
            onPick={(nodeType) => {
              setMenuOpen(false);
              onAddFrom(ports[0] ?? "out", nodeType);
            }}
          />
        </PopoverContent>
      </Popover>
    </div>
  );
}
