"use client";

/// キャンバス上の繰り返し（control.map）の枠（BUG-103・D-85）。
///
/// - 枠の上は通常のブロックと同じ見出し（名前・説明・要修正と注意の印）と出入口。左の入口に前のブロックから線を
///   つなぎ、右の出口から繰り返しの後ろへつなぐ（全部の件が終わってから進む）。尻尾の＋は繰り返しの後ろに足す。
/// - 枠の中が、1 件ごとに動かすブロックの置き場。中のブロックは React Flow の子ノード（`parentId`）なので、枠ごと
///   動かすと中も動く。中が空なら真ん中に「中にブロックを追加」、中があれば左下に同じボタンを出し、中の流れの最後に
///   足す。枠の左の点から、中の最初に動くブロックへ破線を引く（1 件ずつここから入る）。
/// - 右下の角で枠を広げられる（中身より小さくはできない・大きさは layout に持つ）。

import * as React from "react";
import { Handle, NodeResizeControl, Position, type NodeProps } from "@xyflow/react";
import { Plus } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import { categoryVar } from "../category-accent";
import { AddNodeMenu } from "./add-node-menu";
import { FRAME, NODE_H, frameHeaderHeight } from "./frames";
import { NodeHeading, PortRow, type NodeCardData } from "./node-card";
import { continuationPort, drawnPorts } from "./ports";

/// 枠の中の最初に動くブロックへ破線を引く点（つなげない・canvas.tsx の線と同じ名前）。
export const EACH_HANDLE = "__each";

export type MapFrameData = NodeCardData & {
  /// 中のブロックの数（入れ子の中は数えない）。
  childCount: number;
  /// ドラッグ中のブロックをここで離すと中に入る（枠を強調する）。
  dropTarget: boolean;
  /// 縮められる下限（中身を囲む大きさ）。
  minWidth: number;
  minHeight: number;
  /// 中の流れの最後に追加する（中が空なら中の最初のブロック）。
  onAddInside: (mapId: string, nodeType: string) => void;
  /// 枠の大きさを変え終えた。
  onResizeEnd: (mapId: string, size: { width: number; height: number }) => void;
  /// 中に置けないブロックの種類と理由。
  refusedInside: Readonly<Record<string, string>>;
};

export function MapFrame({ id, data, selected }: NodeProps & { data: MapFrameData }) {
  const { irNode, errors, onAddFrom } = data;
  const warnings = data.warnings ?? [];
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  const name = irNode.label || entry?.label_ja || irNode.id;
  const header = frameHeaderHeight(irNode);
  const ports = React.useMemo(() => drawnPorts(irNode, undefined, [], true), [irNode]);
  const tailPort = continuationPort(
    irNode.type,
    ports.map((p) => p.port),
  );
  const hasError = errors.length > 0;
  const [tailOpen, setTailOpen] = React.useState(false);
  const accent = categoryVar(entry?.category);
  const empty = data.childCount === 0;

  return (
    <div
      style={{ ["--accent" as string]: accent }}
      className={cn(
        "group/frame relative flex h-full w-full flex-col rounded-2xl border-2 border-dashed text-card-foreground",
        "transition-[box-shadow,border-color,background-color] duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
        selected && "shadow-lg ring-2 ring-primary",
        hasError
          ? "border-destructive"
          : !hasError && warnings.length > 0
            ? "border-[var(--season-autumn)]"
            : "border-[color-mix(in_oklab,var(--accent)_45%,var(--border))]",
        data.dropTarget && "border-solid border-primary ring-4 ring-primary/25",
      )}
      data-frame={id}
      data-drop-target={data.dropTarget ? "true" : undefined}
    >
      <Handle
        type="target"
        position={Position.Left}
        style={{ top: header / 2 }}
        className="!size-2.5 !border-2 !border-background !bg-muted-foreground transition-transform duration-[var(--duration-fast)] hover:!scale-125 pointer-coarse:!size-4"
      />
      {/* 見出し（通常のブロックと同じ並び）。出口が 2 つ以上なら名前付きの行で並べる。 */}
      <div className="shrink-0 rounded-t-[14px] border-b border-dashed bg-card" style={{ height: header }}>
        <NodeHeading irNode={irNode} errors={errors} warnings={warnings} />
        {ports.length === 1 ? (
          <Handle
            id={ports[0].port}
            type="source"
            position={Position.Right}
            style={{ top: header / 2 }}
            className="!size-2.5 !border-2 !border-background !bg-primary transition-transform duration-[var(--duration-fast)] hover:!scale-125 pointer-coarse:!size-4"
          />
        ) : (
          <div className="border-t px-3.5 py-1.5">
            {ports.map((p) => (
              <PortRow key={p.port} node={irNode} port={p} />
            ))}
          </div>
        )}
      </div>

      {/* 中の流れの入口の点（1 件ずつここから中の最初のブロックへ入る・つなげない）。 */}
      <Handle
        id={EACH_HANDLE}
        type="source"
        position={Position.Right}
        isConnectable={false}
        style={{ top: header + FRAME.padTop + NODE_H / 2, left: FRAME.padX / 2 - 4, right: "auto" }}
        className="!pointer-events-none !size-2 !min-h-0 !min-w-0 !border-0 !bg-[color-mix(in_oklab,var(--accent)_60%,transparent)]"
      />

      <div
        className="relative min-h-0 flex-1 rounded-b-[14px]"
        style={{ backgroundColor: "color-mix(in oklab, var(--accent) 4%, transparent)" }}
      >
        <p className="pointer-events-none absolute left-3.5 top-1 text-[11px] text-muted-foreground">
          リストの 1 件ずつに、中のブロックを実行します。
        </p>
        {empty ? (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 px-6 pt-4 text-center">
            <InsideButton id={id} name={name} data={data} prominent />
            <p className="pointer-events-none max-w-72 text-[11px] leading-relaxed text-muted-foreground">
              1 件ごとに行う処理を、ここに置きます。中のブロックでは「繰り返し中の 1 件」を値に使えます。
            </p>
          </div>
        ) : (
          <div className="absolute bottom-3 left-3.5">
            <InsideButton id={id} name={name} data={data} />
          </div>
        )}
        {data.dropTarget ? (
          <p className="pointer-events-none absolute bottom-3 right-10 rounded-full bg-primary px-2 py-0.5 text-[11px] font-medium text-primary-foreground">
            離すと中に入ります
          </p>
        ) : null}
      </div>

      {/* 尻尾の＋（繰り返しの後ろに追加・全部の件が終わってから動く）。 */}
      <Popover open={tailOpen} onOpenChange={setTailOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={`「${name}」の後ろにブロックを追加`}
            style={{ top: header / 2 }}
            className={cn(
              "nodrag absolute -right-9 flex size-6 -translate-y-1/2 items-center justify-center pointer-coarse:-right-11 pointer-coarse:size-8",
              "rounded-full border bg-background text-muted-foreground shadow-sm",
              "transition-all duration-fast pointer-fine:opacity-60 pointer-fine:group-hover/frame:opacity-100 focus-visible:opacity-100",
              "hover:border-primary hover:bg-primary hover:text-primary-foreground hover:scale-110",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              tailOpen && "opacity-100 border-primary bg-primary text-primary-foreground",
            )}
          >
            <Plus className="size-4" aria-hidden />
          </button>
        </PopoverTrigger>
        <PopoverContent side="right" align="start" className="w-auto p-3">
          <AddNodeMenu
            contextLabel={`「${name}」の後ろに追加（全部の件が終わってから動きます）`}
            refused={data.refusedTypes}
            onPick={(nodeType) => {
              setTailOpen(false);
              onAddFrom(irNode.id, tailPort, nodeType);
            }}
          />
        </PopoverContent>
      </Popover>

      <NodeResizeControl
        position="bottom-right"
        minWidth={data.minWidth}
        minHeight={data.minHeight}
        onResizeEnd={(_, p) => data.onResizeEnd(id, { width: p.width, height: p.height })}
        className="!z-10 !size-5 !rounded-br-2xl !border-0 !bg-transparent"
      >
        <span
          aria-hidden
          className={cn(
            "absolute bottom-1 right-1 size-2.5 border-b-2 border-r-2 border-muted-foreground/60 transition-opacity duration-fast",
            selected ? "opacity-100" : "opacity-0 group-hover/frame:opacity-100",
          )}
          title="枠の大きさを変える"
        />
      </NodeResizeControl>
    </div>
  );
}

/// 「中にブロックを追加」のボタン（押すとブロックを選ぶメニューを開く）。
function InsideButton({
  id,
  name,
  data,
  prominent = false,
}: {
  id: string;
  name: string;
  data: MapFrameData;
  prominent?: boolean;
}) {
  const [open, setOpen] = React.useState(false);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={`「${name}」の中にブロックを追加`}
          // 押しても枠を選んだり拡大したりしない（メニューを開くだけ）。
          onClick={(e) => e.stopPropagation()}
          className={cn(
            "nodrag nopan inline-flex items-center gap-1.5 rounded-full border bg-background font-medium shadow-sm outline-none",
            "transition-colors duration-fast hover:border-primary hover:text-primary focus-visible:ring-2 focus-visible:ring-ring",
            prominent ? "h-9 px-4 text-sm" : "h-7 px-3 text-xs text-muted-foreground",
            open && "border-primary text-primary",
          )}
          data-testid="map-add-inside"
        >
          <Plus className={prominent ? "size-4" : "size-3.5"} aria-hidden />
          中にブロックを追加
        </button>
      </PopoverTrigger>
      <PopoverContent side="bottom" align="start" className="w-auto p-3">
        <AddNodeMenu
          contextLabel={`「${name}」の中に追加`}
          refused={data.refusedInside}
          onPick={(nodeType) => {
            setOpen(false);
            data.onAddInside(id, nodeType);
          }}
        />
      </PopoverContent>
    </Popover>
  );
}
