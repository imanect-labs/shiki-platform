import * as React from "react";

import { cn } from "@/lib/utils";

/// ローディングプレースホルダ。装飾なので支援技術には隠す（aria-hidden）。
/// 親コンテナ側に `aria-busy` を付けてロード状態を伝えること。
function Skeleton({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="skeleton"
      aria-hidden
      className={cn("animate-pulse rounded-md bg-muted", className)}
      {...props}
    />
  );
}

export { Skeleton };
