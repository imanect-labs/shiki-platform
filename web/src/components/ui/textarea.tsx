"use client";

import * as React from "react";

import { cn } from "@/lib/utils";

/// 複数行テキスト入力。`aria-invalid` でエラー時の見た目（リング/ボーダー）を切り替える。
/// prompt-kit の PromptInput 内では focus ring を無効化して使う（コンポーザ枠側で表現）。
function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "flex min-h-16 w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-xs",
        "transition-[color,box-shadow] duration-[var(--duration-fast)]",
        "placeholder:text-muted-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:cursor-not-allowed disabled:opacity-50",
        "aria-[invalid=true]:border-destructive aria-[invalid=true]:focus-visible:ring-destructive",
        className,
      )}
      {...props}
    />
  );
}

export { Textarea };
