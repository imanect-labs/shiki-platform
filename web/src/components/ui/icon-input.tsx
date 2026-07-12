"use client";

/// 先頭アイコン＋クリアボタン付きテキスト入力。ドライブ/共有/検索で個別実装されていた
/// 「検索アイコン＋× で消す」入力を 1 つに集約する。基本の見た目は Input と揃える。

import * as React from "react";
import { X, type LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";

interface Props extends Omit<React.ComponentProps<"input">, "value" | "onChange"> {
  icon?: LucideIcon;
  value: string;
  onValueChange: (value: string) => void;
  /// クリア（×）ボタンのアクセシブル名。
  clearLabel?: string;
  containerClassName?: string;
}

export const IconInput = React.forwardRef<HTMLInputElement, Props>(function IconInput(
  { icon: Icon, value, onValueChange, clearLabel = "クリア", className, containerClassName, ...props },
  ref,
) {
  return (
    <div className={cn("relative flex items-center", containerClassName)}>
      {Icon ? (
        <Icon
          className="pointer-events-none absolute left-3 size-4 text-muted-foreground"
          aria-hidden
        />
      ) : null}
      <input
        ref={ref}
        value={value}
        onChange={(e) => onValueChange(e.target.value)}
        data-slot="input"
        className={cn(
          "flex h-9 w-full rounded-md border border-input bg-background py-1 text-sm shadow-xs",
          "transition-[color,box-shadow] duration-[var(--duration-fast)]",
          "placeholder:text-muted-foreground",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
          "disabled:cursor-not-allowed disabled:opacity-50",
          Icon ? "pl-9" : "pl-3",
          value ? "pr-9" : "pr-3",
          className,
        )}
        {...props}
      />
      {value ? (
        <button
          type="button"
          onClick={() => onValueChange("")}
          aria-label={clearLabel}
          className={cn(
            "absolute right-2 flex size-6 items-center justify-center rounded-md text-muted-foreground outline-none",
            "transition-colors duration-[var(--duration-fast)] hover:bg-accent hover:text-foreground",
            "focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94]",
          )}
        >
          <X className="size-3.5" aria-hidden />
        </button>
      ) : null}
    </div>
  );
});
