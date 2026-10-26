"use client";

import * as React from "react";
import Link from "next/link";
import type { LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ActiveIndicator } from "@/components/ui/motion-primitives";

type BaseProps = {
  icon: LucideIcon;
  label: string;
  active?: boolean;
  /// レール（アイコンのみ）表示か。true ならラベルを隠し tooltip を出す。
  collapsed?: boolean;
  /// 階層の深さ（アコーディオン子のインデント用）。
  depth?: number;
  /// 指定すると、アクティブ時にアイコンを四季の差し色（春→夏→秋→冬の巡回）で点灯させる。
  /// Drive 配下のサブセクションにだけ渡し、汎用ナビは無地のまま据え置く。
  seasonIndex?: number;
  trailing?: React.ReactNode;
  /// 件数のバッジ（承認の受信箱など）。広げた表示では右端に数、レールではアイコンの右上に点を出す。
  badge?: NavBadge | null;
  className?: string;
};

/// 件数のバッジ。`text` はピルに出す字、`spoken` は読み上げ・ツールチップの「ラベル（…）」の括弧の中に入れる語
/// （例: `{ text: "3", spoken: "3 件" }`）。
export type NavBadge = { text: string; spoken: string };

type LinkProps = BaseProps & { href: string; onClick?: () => void };
type ButtonProps = BaseProps & {
  href?: undefined;
  onClick?: () => void;
  "aria-expanded"?: boolean;
  "aria-controls"?: string;
};

// アクティブ＝ホバーと同系のグレー（白カードは廃止）。アクティブはやや濃いグレーで区別。
function itemClasses(active: boolean, collapsed: boolean, depth: number) {
  return cn(
    // isolate: 独自の stacking context を作り、アクティブピル（-z-10）を項目内に収める
    // （サイドバー背景の下へ潜って見えなくなる事故を防ぐ）。
    "group/navitem relative isolate flex h-9 items-center gap-2.5 rounded-[9px] text-[13.5px] outline-none transition-colors",
    "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
    // タッチ（md 以上のタブレット等）は行の当たり判定を 44px に広げる。
    collapsed ? "w-9 justify-center px-0" : "w-full px-2.5 pointer-coarse:h-11",
    active
      ? "font-medium text-sidebar-foreground"
      : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
    !collapsed && depth > 0 && "pl-9",
  );
}

function Inner({
  icon: Icon,
  label,
  active,
  collapsed,
  seasonIndex,
  trailing,
  badge,
}: {
  icon: LucideIcon;
  label: string;
  active: boolean;
  collapsed: boolean;
  seasonIndex?: number;
  trailing?: React.ReactNode;
  badge?: NavBadge | null;
}) {
  // アクティブかつ季節指定がある場合だけアイコンを季節色に。それ以外は無地。
  const seasonTint = active && seasonIndex != null;
  return (
    <>
      {/* アクティブをアイテム間でスライドする塗りピル（layoutId・唯一の動く要素）。
          細い縦線ではなく丸角の面が滑ることで安っぽさを避ける。季節色はアイコン側で示す。 */}
      {active ? (
        <ActiveIndicator
          layoutId="sidebar-active-nav"
          className="absolute inset-0 -z-10 rounded-[9px] bg-sidebar-accent"
        />
      ) : null}
      <Icon
        className={cn(
          "size-[18px] shrink-0 transition-transform duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
          // ホバーで軽くアイコンをずらす（微インタラクション・CSS のみ）。
          "group-hover/navitem:translate-x-0.5",
          seasonTint ? "" : active ? "text-sidebar-foreground" : "text-sidebar-foreground/55",
        )}
        style={seasonTint ? { color: seasonVar(seasonIndex) } : undefined}
        strokeWidth={2}
      />
      {collapsed && badge ? (
        <span
          aria-hidden
          className="absolute right-1.5 top-1.5 size-2 rounded-full bg-primary ring-2 ring-sidebar"
        />
      ) : null}
      {!collapsed ? (
        <>
          <span className="flex-1 truncate text-left">{label}</span>
          {badge ? <CountBadge value={badge.text} /> : null}
          {trailing}
        </>
      ) : null}
    </>
  );
}

/// 件数のピル（広げた表示の右端）。
export function CountBadge({ value, className }: { value: string; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex h-[18px] min-w-[18px] shrink-0 items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-semibold leading-none text-primary-foreground tabular-nums",
        className,
      )}
    >
      {value}
    </span>
  );
}

/// サイドバーの 1 行。`href` があれば Link、無ければ button として描画する。
/// レール時はアイコンのみ＋tooltip でラベルを補う（a11y のため aria-label も付与）。
export function NavItem(props: LinkProps | ButtonProps) {
  const {
    icon,
    label,
    active = false,
    collapsed = false,
    depth = 0,
    seasonIndex,
    trailing,
    badge,
    className,
  } = props;
  // 件数は読み上げのラベルにも含める（点・数字は aria-hidden）。
  const spoken = badge ? `${label}（${badge.spoken}）` : label;

  const node =
    "href" in props && props.href !== undefined ? (
      <Link
        href={props.href}
        onClick={props.onClick}
        aria-current={active ? "page" : undefined}
        aria-label={collapsed || badge ? spoken : undefined}
        className={cn(itemClasses(active, collapsed, depth), className)}
      >
        <Inner icon={icon} label={label} active={active} collapsed={collapsed} seasonIndex={seasonIndex} trailing={trailing} badge={badge} />
      </Link>
    ) : (
      <button
        type="button"
        onClick={props.onClick}
        aria-current={active ? "page" : undefined}
        aria-label={collapsed || badge ? spoken : undefined}
        aria-expanded={props["aria-expanded"]}
        aria-controls={props["aria-controls"]}
        className={cn(itemClasses(active, collapsed, depth), className)}
      >
        <Inner icon={icon} label={label} active={active} collapsed={collapsed} seasonIndex={seasonIndex} trailing={trailing} badge={badge} />
      </button>
    );

  if (!collapsed) return node;

  return (
    <Tooltip>
      <TooltipTrigger asChild>{node}</TooltipTrigger>
      <TooltipContent side="right">{spoken}</TooltipContent>
    </Tooltip>
  );
}
