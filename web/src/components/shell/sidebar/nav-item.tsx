"use client";

import * as React from "react";
import Link from "next/link";
import type { LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

type BaseProps = {
  icon: LucideIcon;
  label: string;
  active?: boolean;
  /// レール（アイコンのみ）表示か。true ならラベルを隠し tooltip を出す。
  collapsed?: boolean;
  /// 階層の深さ（アコーディオン子のインデント用）。
  depth?: number;
  trailing?: React.ReactNode;
  className?: string;
};

type LinkProps = BaseProps & { href: string; onClick?: () => void };
type ButtonProps = BaseProps & {
  href?: undefined;
  onClick?: () => void;
  "aria-expanded"?: boolean;
  "aria-controls"?: string;
};

function itemClasses(active: boolean, collapsed: boolean, depth: number) {
  return cn(
    "group/navitem relative flex h-9 items-center gap-3 rounded-md text-sm font-medium outline-none transition-colors",
    "focus-visible:ring-2 focus-visible:ring-sidebar-ring",
    collapsed ? "w-9 justify-center px-0" : "w-full px-3",
    active
      ? "bg-sidebar-accent text-sidebar-accent-foreground"
      : "text-sidebar-foreground/80 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
    !collapsed && depth > 0 && "pl-9",
  );
}

/// アクティブ時の左アクセントバー。
function ActiveBar({ active }: { active: boolean }) {
  return (
    <span
      aria-hidden
      className={cn(
        "absolute left-0 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-full bg-primary transition-opacity",
        active ? "opacity-100" : "opacity-0",
      )}
    />
  );
}

function Inner({
  icon: Icon,
  label,
  collapsed,
  trailing,
}: {
  icon: LucideIcon;
  label: string;
  collapsed: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <>
      <Icon className="size-4 shrink-0" aria-hidden />
      {!collapsed ? (
        <>
          <span className="flex-1 truncate text-left">{label}</span>
          {trailing}
        </>
      ) : null}
    </>
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
    trailing,
    className,
  } = props;

  const node =
    "href" in props && props.href !== undefined ? (
      <Link
        href={props.href}
        onClick={props.onClick}
        aria-current={active ? "page" : undefined}
        aria-label={collapsed ? label : undefined}
        className={cn(itemClasses(active, collapsed, depth), className)}
      >
        <ActiveBar active={active} />
        <Inner icon={icon} label={label} collapsed={collapsed} trailing={trailing} />
      </Link>
    ) : (
      <button
        type="button"
        onClick={props.onClick}
        aria-current={active ? "page" : undefined}
        aria-label={collapsed ? label : undefined}
        aria-expanded={props["aria-expanded"]}
        aria-controls={props["aria-controls"]}
        className={cn(itemClasses(active, collapsed, depth), className)}
      >
        <ActiveBar active={active} />
        <Inner icon={icon} label={label} collapsed={collapsed} trailing={trailing} />
      </button>
    );

  if (!collapsed) return node;

  // レール時はラベルを tooltip で補完。
  return (
    <Tooltip>
      <TooltipTrigger asChild>{node}</TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}
