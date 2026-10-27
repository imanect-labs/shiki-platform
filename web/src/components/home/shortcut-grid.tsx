"use client";

import Link from "next/link";

import { cn } from "@/lib/utils";
import {
  HOME_SHORTCUT_CATEGORIES,
  type HomeShortcut,
} from "@/lib/home-shortcuts";
import { seasonVar } from "@/lib/season";

/// ホーム下部の機能ショートカット群（カテゴリ＝縦区切りで横並び、画像1 風）。
/// 候補チップと揃えて、ホバー時にタイルを四季の差し色で点灯させる（全タイル通しの巡回）。
export function ShortcutGrid() {
  return (
    <div className="flex flex-wrap justify-center gap-y-6">
      {HOME_SHORTCUT_CATEGORIES.map((category, ci) => {
        // 全カテゴリを通した連番で季節を割り当てる（春→夏→秋→冬の巡回を画面全体で揃える）。
        const offset = HOME_SHORTCUT_CATEGORIES.slice(0, ci).reduce(
          (n, c) => n + c.items.length,
          0,
        );
        return (
          <section
            key={category.key}
            className={cn("px-5", ci > 0 && "shiki-divide-l")}
          >
            <h3 className="mb-3 text-center text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">
              {category.label}
            </h3>
            <div className="flex justify-center gap-1">
              {category.items.map((item, ii) => (
                <ShortcutItem key={item.key} item={item} seasonIndex={offset + ii} />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function ShortcutItem({ item, seasonIndex }: { item: HomeShortcut; seasonIndex: number }) {
  const { icon: Icon, label, href } = item;
  return (
    <Link
      href={href}
      aria-label={label}
      className="rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <span className="group flex w-[76px] flex-col items-center gap-2 rounded-xl py-2 text-center outline-none">
        <span
          style={{ ["--season" as string]: seasonVar(seasonIndex) }}
          className="flex size-11 items-center justify-center rounded-2xl border border-border bg-card text-foreground/75 transition-colors group-hover:border-[var(--season)]/45 group-hover:bg-[var(--season)]/[0.08] group-hover:text-[var(--season)]"
        >
          <Icon className="size-[19px]" aria-hidden />
        </span>
        <span className="text-[12px] leading-none text-foreground/75 group-hover:text-foreground">
          {label}
        </span>
      </span>
    </Link>
  );
}
