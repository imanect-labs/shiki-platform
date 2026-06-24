"use client";

import Link from "next/link";

import { cn } from "@/lib/utils";
import {
  HOME_SHORTCUT_CATEGORIES,
  type HomeShortcut,
} from "@/lib/home-shortcuts";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/// ホーム下部の機能ショートカット群（カテゴリ＝縦区切りで横並び、画像1 風）。
/// 実装済みは Link、未実装は「準備中」tooltip 付きの無効ボタン。
export function ShortcutGrid() {
  return (
    <div className="flex flex-wrap justify-center gap-y-6">
      {HOME_SHORTCUT_CATEGORIES.map((category, i) => (
        <section
          key={category.key}
          className={cn(
            "px-5 sm:px-7",
            i > 0 && "sm:border-l sm:border-border",
          )}
        >
          <h3 className="mb-3 text-center text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">
            {category.label}
          </h3>
          <div className="flex justify-center gap-1">
            {category.items.map((item) => (
              <ShortcutItem key={item.key} item={item} />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

function ShortcutItem({ item }: { item: HomeShortcut }) {
  const { icon: Icon, label, ready, href } = item;

  const tile = (
    <span
      className={cn(
        "flex size-11 items-center justify-center rounded-2xl border transition-colors",
        ready
          ? "border-border bg-card text-foreground/75 group-hover:border-ring/40 group-hover:bg-accent group-hover:text-foreground"
          : "border-dashed border-border bg-muted/40 text-muted-foreground/60",
      )}
    >
      <Icon className="size-[19px]" aria-hidden />
    </span>
  );

  const text = (
    <span
      className={cn(
        "text-[12px] leading-none",
        ready ? "text-foreground/75 group-hover:text-foreground" : "text-muted-foreground/60",
      )}
    >
      {label}
    </span>
  );

  const inner = (
    <span className="group flex w-[68px] flex-col items-center gap-2 rounded-xl py-2 text-center outline-none">
      {tile}
      {text}
    </span>
  );

  if (ready && href) {
    return (
      <Link
        href={href}
        aria-label={label}
        className="rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {inner}
      </Link>
    );
  }

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          disabled
          aria-label={`${label}（準備中）`}
          className="cursor-not-allowed rounded-xl outline-none"
        >
          {inner}
        </button>
      </TooltipTrigger>
      <TooltipContent>準備中</TooltipContent>
    </Tooltip>
  );
}
