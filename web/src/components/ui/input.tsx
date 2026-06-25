"use client";

import * as React from "react";

import { cn } from "@/lib/utils";

/// 基本テキスト入力。ラベルの関連付けは呼び出し側の責務。
/// `aria-invalid` でエラー時の見た目（リング/ボーダー）を切り替える。
function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-xs",
        "transition-[color,box-shadow] duration-[var(--duration-fast)]",
        "placeholder:text-muted-foreground",
        "file:border-0 file:bg-transparent file:text-sm file:font-medium",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:cursor-not-allowed disabled:opacity-50",
        "aria-[invalid=true]:border-destructive aria-[invalid=true]:focus-visible:ring-destructive",
        className,
      )}
      {...props}
    />
  );
}

export { Input };
