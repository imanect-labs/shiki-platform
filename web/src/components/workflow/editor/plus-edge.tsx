"use client";

/// エッジ（中点の挿入プラスボタン付き・ポート名ラベル）。
/// マウスではホバーで現れ、タッチ端末（hover が無い）では常時表示・大きめにする。
/// クリックで選択でき（線を太く・主色に）、選んでいる間は削除ボタンを出す（Delete キーが無い端末向け）。

import * as React from "react";
import {
  BaseEdge,
  EdgeLabelRenderer,
  getBezierPath,
  type EdgeProps,
} from "@xyflow/react";
import { Plus, Trash2 } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import type { Edge as IrEdge } from "@/generated/workflow-ir";
import { AddNodeMenu } from "./add-node-menu";
import { portLabel } from "../port-labels";

export type PlusEdgeData = {
  irEdge: IrEdge;
  /// このエッジ上に新ノードを割り込ませる。
  onInsert: (nodeType: string, position: { x: number; y: number }) => void;
  /// このつながりを消す（選択中に出す削除ボタン）。
  onDelete: () => void;
  errorMessages: string[];
  [key: string]: unknown;
};

/// 出口のラベルを＋の隣に出せるか（BUG-41）。出口のノードの尻尾の＋（カードの右 12〜36px・高さはカードの
/// 中ほど）にラベルと＋の組が重なるなら出さない（出口の名前はカードの出口の並びにも出ている）。
/// 座標はキャンバスの座標（拡大縮小の前）。幅は 10px の文字・左右の余白・＋（20px）・間（4px）の見積もり。
export function portLabelFits(p: { sourceX: number; sourceY: number; labelX: number; labelY: number; text: string }): boolean {
  const TAIL_RIGHT = 40;
  const NEAR_Y = 48;
  const width = [...p.text].length * 10 + 14 + 4 + 20;
  return p.labelX - width / 2 >= p.sourceX + TAIL_RIGHT || Math.abs(p.labelY - p.sourceY) >= NEAR_Y;
}

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
  // 既定の出口（次へ）はつながりに名前を出さない（どのノードにもある・カードの出口の並びに出ている）。
  const portName = port === "out" ? "" : portLabel(port);
  // 短いつながりではラベルを出さない（尻尾の＋に重なる・BUG-41）。選んでいる間は＋の上に重ねずに出す。
  const portText =
    portName && (selected || portLabelFits({ sourceX, sourceY, labelX, labelY, text: portName })) ? portName : "";

  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        className={cn(
          selected ? "!stroke-[2.5]" : "!stroke-[1.5]",
          hasError
            ? "!stroke-destructive"
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
          <div className="group/edge relative flex items-center gap-1">
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
                    "flex size-5 items-center justify-center rounded-full border bg-background text-muted-foreground shadow-sm pointer-coarse:size-7",
                    "transition-opacity duration-fast pointer-fine:opacity-0 pointer-fine:group-hover/edge:opacity-100 focus-visible:opacity-100",
                    "hover:border-primary hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    // 開いている間・選択中は常に出す（`pointer-fine:` 付きで上書きしないと hover 前の透明が勝つ）。
                    (menuOpen || selected) && "pointer-fine:opacity-100",
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
            {selected ? (
              // ＋の位置を動かさないよう、＋の右隣に重ねて置く。
              <button
                type="button"
                aria-label="このつながりを削除"
                onClick={data.onDelete}
                className={cn(
                  "absolute left-full ml-1 flex size-5 items-center justify-center rounded-full border bg-background text-muted-foreground shadow-sm pointer-coarse:ml-1.5 pointer-coarse:size-7",
                  "hover:border-destructive hover:text-destructive focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                )}
              >
                <Trash2 className="size-3" aria-hidden />
              </button>
            ) : null}
          </div>
        </div>
      </EdgeLabelRenderer>
    </>
  );
}
