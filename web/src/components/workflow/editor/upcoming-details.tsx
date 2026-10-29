"use client";

/// 「準備中」のブロックを畳んでおく見出し（パレットと、線の＋から開く追加のメニューで共用・付録 D の N2.10）。
///
/// - 開けることが見えるよう、回転する印を付ける（Safari が既定の印を消さずに残すので、既定の印は消す）。
/// - 検索しているあいだだけ開いておく。検索していないときは、利用者が開けた・閉じた状態を保つ（検索を消しても勝手に
///   閉じない）。検索で準備中のブロックが無くなるとこの見出しは消えるので、開閉の状態は呼び出し側が持つ
///   （[`useUpcomingOpen`]）。

import * as React from "react";
import { ChevronRight } from "lucide-react";

import { cn } from "@/lib/utils";

/// 利用者が開けた・閉じた状態（検索していないときの開閉）。
export function useUpcomingOpen(): [boolean, (open: boolean) => void] {
  return React.useState(false);
}

export function UpcomingDetails({
  searching,
  open,
  onOpenChange,
  className,
  summaryClassName,
  children,
}: {
  /// 検索の語が入っているか（そのあいだは開いておく）。
  searching: boolean;
  /// 利用者が開けたか（[`useUpcomingOpen`]）。
  open: boolean;
  onOpenChange: (open: boolean) => void;
  className?: string;
  summaryClassName?: string;
  children: React.ReactNode;
}) {
  return (
    <details
      className={cn("group", className)}
      open={searching || open}
      onToggle={(e) => {
        if (!searching) onOpenChange(e.currentTarget.open);
      }}
    >
      <summary
        className={cn(
          "flex cursor-pointer list-none items-center gap-1 rounded-md text-[11px] font-medium text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden",
          summaryClassName,
        )}
      >
        <ChevronRight className="size-3 shrink-0 transition-transform duration-fast group-open:rotate-90" aria-hidden />
        準備中
      </summary>
      {children}
    </details>
  );
}
