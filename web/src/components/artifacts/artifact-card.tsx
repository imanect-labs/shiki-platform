"use client";

/// スキル／アプリ一覧のカード（名前・版・主ボタン 1 つ＋⋯）。
///
/// 操作を横に並べるとスマホで 2〜3 段に折り返すため、主操作だけを外に出し、残り（編集・共有・
/// 公開・版・削除）は ⋯ に畳む。PC も同じ形にそろえる（カードの見た目を画面幅で変えない）。

import * as React from "react";
import { Loader2, MoreHorizontal, type LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export type CardMenuItem = {
  key: string;
  label: string;
  icon: LucideIcon;
  onSelect: () => void;
  disabled?: boolean;
  destructive?: boolean;
  separatorBefore?: boolean;
};

export function ArtifactCard({
  name,
  meta,
  pending,
  primary,
  menu,
  className,
}: {
  name: string;
  meta: React.ReactNode;
  pending?: boolean;
  /// 主ボタン（1 つ）。
  primary: React.ReactNode;
  menu: CardMenuItem[];
  className?: string;
}) {
  return (
    <li
      className={cn(
        "flex flex-col gap-3 rounded-xl border border-border bg-card p-4 transition-shadow hover:shadow-[var(--elevation-1,0_1px_3px_rgb(0_0_0/0.08))]",
        className,
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <h2 className="truncate text-[15px] font-semibold">{name}</h2>
          <p className="text-xs text-muted-foreground">{meta}</p>
        </div>
        {pending ? (
          <Loader2 className="mt-0.5 size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
        ) : null}
      </div>
      <div className="mt-auto flex items-center gap-2">
        <div className="min-w-0 flex-1 [&>*]:max-w-full">{primary}</div>
        {menu.length > 0 ? (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                size="icon"
                variant="ghost"
                aria-label={`${name} のその他の操作`}
                className="shrink-0 text-muted-foreground"
              >
                <MoreHorizontal className="size-4" aria-hidden />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-52">
              {menu.map((item) => {
                const Icon = item.icon;
                return (
                  <React.Fragment key={item.key}>
                    {item.separatorBefore ? <DropdownMenuSeparator /> : null}
                    <DropdownMenuItem
                      disabled={item.disabled}
                      onSelect={item.onSelect}
                      className={cn(
                        "gap-2.5",
                        item.destructive &&
                          "text-destructive focus:text-destructive [&_svg]:text-destructive",
                      )}
                    >
                      <Icon className="size-4 text-muted-foreground" aria-hidden />
                      {item.label}
                    </DropdownMenuItem>
                  </React.Fragment>
                );
              })}
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
      </div>
    </li>
  );
}
