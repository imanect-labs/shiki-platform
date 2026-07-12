"use client";

/// フローティング ツールバー（浮遊バー）。md エディタの選択バブル・CSV グリッドの
/// アクションバーなど「面の上に浮くコントロール帯」の共通ガワ。
///
/// - 見た目は他の浮遊面（dropdown/popover/tooltip）と統一: border + bg-popover + shadow-md + rounded-lg。
/// - 入場/退場は FadeSlide（transform/opacity のみ・軽量）。AnimatePresence の子として使うと退場もアニメ。
/// - 中身のボタンは呼び出し側で `ToolbarButton` を並べる想定（区切りは ToolbarSeparator）。

import * as React from "react";

import { cn } from "@/lib/utils";
import { FadeSlide } from "./motion-primitives";

export function FloatingToolbar({
  from = "bottom",
  className,
  children,
  ...props
}: {
  from?: "top" | "bottom" | "left" | "right";
} & React.ComponentProps<typeof FadeSlide>) {
  return (
    <FadeSlide
      from={from}
      role="toolbar"
      className={cn(
        "flex items-center gap-0.5 rounded-lg border bg-popover p-1 text-popover-foreground shadow-md",
        className,
      )}
      {...props}
    >
      {children}
    </FadeSlide>
  );
}

/// ツールバー内のアイコンボタン。active でトグルの ON を示す（bubble menu の書式状態など）。
export const ToolbarButton = React.forwardRef<
  HTMLButtonElement,
  React.ComponentProps<"button"> & { active?: boolean }
>(function ToolbarButton({ active, className, ...props }, ref) {
  return (
    <button
      ref={ref}
      type="button"
      aria-pressed={active}
      className={cn(
        "inline-flex size-8 items-center justify-center rounded-md outline-none",
        "transition-colors duration-[var(--duration-fast)] active:scale-[0.94]",
        "focus-visible:ring-2 focus-visible:ring-ring",
        "[&_svg]:size-4 [&_svg]:shrink-0",
        active
          ? "bg-accent text-accent-foreground"
          : "text-muted-foreground hover:bg-accent hover:text-foreground",
        className,
      )}
      {...props}
    />
  );
});

/// 縦の区切り線。
export function ToolbarSeparator({ className }: { className?: string }) {
  return <span aria-hidden className={cn("mx-0.5 h-5 w-px bg-border", className)} />;
}
