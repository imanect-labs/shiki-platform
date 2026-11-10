"use client";

import { ChevronRight, History } from "lucide-react";

import { cn } from "@/lib/utils";

/// 実行中の会話へ戻ったとき、離れていた間までの操作を畳んだ 1 行（#1152）。押すとその場で開閉する。
///
/// 戻ってきた人が知りたいのは「いま何をしているか」で、途中の手順を 1 件ずつ見直したいわけではない。
/// 見た目は思考プロセスの見出しと揃える（同じ大きさ・色・開閉の印）。
export function PastWorkToggle({
  tools,
  open,
  onToggle,
}: {
  /// 畳んだまとまりに含まれるツール呼び出しの数（0 なら思考だけ）。
  tools: number;
  open: boolean;
  onToggle: () => void;
}) {
  const label = tools > 0 ? `これまでの ${tools} 件の操作` : "これまでの思考";
  return (
    <div className="mb-2.5">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex items-center gap-1.5 rounded-md py-0.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <History className="size-3.5" aria-hidden />
        <span className="font-medium">{open ? `${label}を畳む` : label}</span>
        <ChevronRight
          className={cn("size-3.5 transition-transform", open && "rotate-90")}
          aria-hidden
        />
      </button>
    </div>
  );
}

/// 戻った直後、離れていた間までの経過が届くのを待っている 1 行。
export function PastWorkLoading() {
  return (
    <div className="mb-2.5 flex items-center gap-1.5 py-0.5 text-[13px] text-muted-foreground" role="status">
      <History className="size-3.5 animate-pulse motion-reduce:animate-none" aria-hidden />
      <span className="font-medium animate-pulse motion-reduce:animate-none">
        これまでの経過を読み込んでいます…
      </span>
    </div>
  );
}
