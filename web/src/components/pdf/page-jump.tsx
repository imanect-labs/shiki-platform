"use client";

/// 「3 / 12」のページ表示を兼ねる入力欄。番号を打って Enter（またはフォーカスを外す）で移動する。
import * as React from "react";

import { cn } from "@/lib/utils";

export function PageJump({
  current,
  total,
  onJump,
  className,
  suffix,
}: {
  current: number;
  total: number;
  onJump: (page: number) => void;
  className?: string;
  /// 「/ 12」の後ろに付ける語（PC のバーでは「ページ」）。
  suffix?: string;
}) {
  const [draft, setDraft] = React.useState<string | null>(null);
  const commit = () => {
    if (draft == null) return;
    const n = Number.parseInt(draft.normalize("NFKC"), 10);
    setDraft(null);
    if (Number.isFinite(n)) onJump(Math.min(total, Math.max(1, n)));
  };
  return (
    <span className={cn("inline-flex items-center gap-1 tabular-nums", className)}>
      <input
        value={draft ?? String(current)}
        onChange={(e) => setDraft(e.target.value)}
        onFocus={(e) => {
          setDraft(String(current));
          e.target.select();
        }}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
            e.currentTarget.blur();
          } else if (e.key === "Escape") {
            setDraft(null);
            e.currentTarget.blur();
          }
        }}
        inputMode="numeric"
        aria-label={`ページ番号（全 ${total} ページ）`}
        className={cn(
          "h-6 rounded-md bg-muted/60 px-1 text-center text-[12px] font-medium text-foreground outline-none",
          "hover:bg-muted focus:bg-background focus:ring-2 focus:ring-ring max-md:text-base",
        )}
        // 桁数ぶんの幅（全ページ数の桁 + 少し）。
        style={{ width: `calc(${String(total).length}ch + 14px)` }}
        data-testid="pdf-page-input"
      />
      <span aria-hidden>
        / {total}
        {suffix ? ` ${suffix}` : ""}
      </span>
    </span>
  );
}
