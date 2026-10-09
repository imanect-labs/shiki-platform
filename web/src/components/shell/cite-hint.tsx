"use client";

/// 引用箇所から原本を開いたときの案内（ノート・Office・PDF で共通・#508）。
///
/// エディタの上に小さく浮かべ、どの一節を探したか・見つかったかを伝える。閉じれば消える。
import * as React from "react";
import { Quote, X } from "lucide-react";

import { cn } from "@/lib/utils";

export type CiteHintTone = "found" | "approx" | "missing";

export function CiteHint({
  tone,
  children,
  className,
}: {
  tone: CiteHintTone;
  children: React.ReactNode;
  className?: string;
}) {
  const [open, setOpen] = React.useState(true);
  if (!open) return null;
  return (
    <div
      role="status"
      data-testid="cite-hint"
      data-tone={tone}
      className={cn(
        "pointer-events-auto flex max-w-[min(36rem,calc(100%-2rem))] items-center gap-2 rounded-full border bg-card/95 py-1.5 pl-3 pr-1.5 text-[12.5px] shadow-md shadow-black/[0.06] backdrop-blur",
        tone === "found" ? "border-[var(--season-autumn)]/40" : "border-border",
        className,
      )}
    >
      <Quote
        className={cn(
          "size-3.5 shrink-0",
          tone === "found" ? "text-[var(--season-autumn)]" : "text-muted-foreground",
        )}
        aria-hidden
      />
      <span className="min-w-0 truncate text-foreground/85">{children}</span>
      <button
        type="button"
        onClick={() => setOpen(false)}
        aria-label="案内を閉じる"
        className="flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <X className="size-3.5" aria-hidden />
      </button>
    </div>
  );
}

/// 一節を「…」で短く見せる。
export function shortPhrase(s: string, max = 24): string {
  const chars = Array.from(s.trim());
  return chars.length > max ? `${chars.slice(0, max).join("")}…` : chars.join("");
}
