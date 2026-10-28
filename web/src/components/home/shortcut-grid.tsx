"use client";

import * as React from "react";
import Link from "next/link";

import {
  HOME_SHORTCUT_CATEGORIES,
  type HomeShortcut,
} from "@/lib/home-shortcuts";
import { seasonVar } from "@/lib/season";

/// ホーム下部の機能ショートカット群（カテゴリ＝縦区切りで横並び、画像1 風）。
/// 幅が足りず縦に積むときは区切りを出さない（折り返した行の左に破線が残らないように）。
/// 候補チップと揃えて、ホバー時にタイルを四季の差し色で点灯させる（全タイル通しの巡回）。
export function ShortcutGrid() {
  return (
    <div className="@container w-full">
      <div className="flex flex-col items-center gap-6 @min-[640px]:flex-row @min-[640px]:items-stretch @min-[640px]:justify-center @min-[640px]:gap-0">
        {HOME_SHORTCUT_CATEGORIES.map((category, ci) => {
          // 全カテゴリを通した連番で季節を割り当てる（春→夏→秋→冬の巡回を画面全体で揃える）。
          const offset = HOME_SHORTCUT_CATEGORIES.slice(0, ci).reduce(
            (n, c) => n + c.items.length,
            0,
          );
          return (
            <React.Fragment key={category.key}>
              {ci > 0 ? (
                <span
                  aria-hidden
                  className="shiki-divide-l hidden w-[1.5px] shrink-0 @min-[640px]:block"
                />
              ) : null}
              <section className="px-5">
                <h3 className="mb-3 text-center text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/70">
                  {category.label}
                </h3>
                <div className="flex justify-center gap-1">
                  {category.items.map((item, ii) => (
                    <ShortcutItem key={item.key} item={item} seasonIndex={offset + ii} />
                  ))}
                </div>
              </section>
            </React.Fragment>
          );
        })}
      </div>
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
