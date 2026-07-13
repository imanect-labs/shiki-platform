"use client";

/// セグメント コントロール（タブ的トグル）。アクティブなサム（thumb）が layoutId で
/// 選択肢の間を滑らかにスライドする。各所でハンドロールされていた list/grid トグルや
/// grid/SQL 切替、共有ダイアログの種別トグルを 1 つに集約する。
///
/// - motion は「サムのスライド」1 要素だけに使う（軽量・見せ場限定）。reduced-motion は
///   providers/globals の二重セーフティで停止する。
/// - a11y: role="tablist"/"tab"＋aria-selected。値は文字列。

import * as React from "react";
import { motion } from "motion/react";
import type { LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { EASE_STANDARD, DURATION_NORMAL } from "./motion-primitives";

export interface SegmentedOption {
  value: string;
  label: string;
  icon?: LucideIcon;
  /// E2E 等のための data-testid（任意）。
  testId?: string;
}

interface Props {
  options: SegmentedOption[];
  value: string;
  onValueChange: (value: string) => void;
  /// スクリーンリーダ向けのグループ名。
  "aria-label": string;
  size?: "sm" | "default";
  className?: string;
}

export function SegmentedControl({
  options,
  value,
  onValueChange,
  size = "default",
  className,
  ...aria
}: Props) {
  // layoutId 衝突を避けるためインスタンス固有の id を使う。
  const groupId = React.useId();

  return (
    <div
      role="tablist"
      aria-label={aria["aria-label"]}
      className={cn(
        "relative inline-flex items-center gap-0.5 rounded-lg border bg-muted/40 p-0.5",
        className,
      )}
    >
      {options.map((o) => {
        const active = o.value === value;
        const Icon = o.icon;
        return (
          <button
            key={o.value}
            type="button"
            role="tab"
            aria-selected={active}
            data-testid={o.testId}
            onClick={() => onValueChange(o.value)}
            className={cn(
              "relative z-10 inline-flex items-center justify-center gap-1.5 rounded-md font-medium outline-none",
              "transition-colors duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
              "focus-visible:ring-2 focus-visible:ring-ring",
              size === "sm" ? "h-6 px-2 text-[11px]" : "h-7 px-3 text-xs",
              active ? "text-foreground" : "text-muted-foreground hover:text-foreground",
            )}
          >
            {active ? (
              <motion.span
                layoutId={`segmented-thumb-${groupId}`}
                aria-hidden
                className="absolute inset-0 -z-10 rounded-md border bg-background shadow-xs"
                transition={{ duration: DURATION_NORMAL, ease: EASE_STANDARD }}
              />
            ) : null}
            {Icon ? <Icon className="size-3.5" aria-hidden /> : null}
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
