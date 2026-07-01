"use client";

import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { cn } from "@/lib/utils";

/// ホームのコンポーザへ視線をやさしく誘う、手描き風の点線矢印。装飾なので aria-hidden。
/// 差し色は葉っぱの区切りと同じ「今の季節」。アプリ全体の荒い破線言語に合わせ、軌跡は破線・
/// 矢じりだけ実線にして先端をはっきりさせる（矢じりは終点 17,80 の接線方向に開く直線 2 本）。
export function ComposerArrow({ className }: { className?: string }) {
  return (
    <div
      className={cn("pointer-events-none select-none opacity-90", className)}
      style={{ color: seasonVar(currentSeasonIndex()) }}
      aria-hidden
    >
      <svg width="148" height="78" viewBox="0 0 180 95" fill="none" className="overflow-visible">
        {/* 右上から左下へ、途中で一度くるりと回るやわらかい軌跡（破線） */}
        <path
          d="M 168 17 C 138 7, 105 18, 88 39 C 78 52, 87 66, 101 58 C 115 50, 107 28, 87 28 C 58 28, 31 47, 17 80"
          stroke="currentColor"
          strokeWidth="4.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeDasharray="7 9"
        />
        {/* 矢じり（実線・終点で開く直線 2 本） */}
        <path
          d="M 17 80 L 30 70 M 17 80 L 15 64"
          stroke="currentColor"
          strokeWidth="4.5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </div>
  );
}
