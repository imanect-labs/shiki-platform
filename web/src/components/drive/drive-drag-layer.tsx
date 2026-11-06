"use client";

import * as React from "react";
import { createPortal } from "react-dom";
import { CornerDownRight } from "lucide-react";

import { NodeIcon } from "./primitives";
import { useDragState } from "./use-drive-dnd";

/// ドラッグ中、カーソルの右下に浮くカード（掴んでいる物を見せる）。
/// ブラウザ既定のドラッグイメージ（行の薄い写し）は use-drive-dnd 側で透明にしてある。
/// 移動先に乗っている間は「→ ○○へ移動」を添える。ポインタを奪わない（pointer-events-none）。
export function DriveDragLayer() {
  const drag = useDragState();
  const [mounted, setMounted] = React.useState(false);
  React.useEffect(() => setMounted(true), []);
  if (!mounted || !drag || drag.x === null || drag.y === null) return null;

  return createPortal(
    <div
      aria-hidden
      className="pointer-events-none fixed left-0 top-0 z-[100]"
      style={{ transform: `translate(${drag.x + 14}px, ${drag.y + 10}px)` }}
    >
      <div
        className="flex max-w-72 animate-in fade-in-0 zoom-in-95 flex-col gap-1 rounded-lg border border-border bg-popover px-3 py-2 text-popover-foreground shadow-xl ring-1 ring-black/5 duration-150 motion-safe:-rotate-2"
      >
        <div className="flex min-w-0 items-center gap-2.5">
          <NodeIcon
            kind={drag.node.kind}
            name={drag.node.name}
            contentType={drag.contentType}
            className="size-5 shrink-0"
          />
          <span className="truncate text-[13.5px] font-medium">{drag.node.name}</span>
        </div>
        {drag.target ? (
          <div className="flex min-w-0 items-center gap-1 pl-0.5 text-[12px] text-primary">
            <CornerDownRight className="size-3.5 shrink-0" />
            <span className="truncate">「{drag.target}」へ移動</span>
          </div>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}
