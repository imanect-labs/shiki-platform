"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ChevronRight } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  DRIVE_CHILDREN,
  DRIVE_ICON,
  DRIVE_ROOT,
  isActivePath,
} from "@/lib/nav-config";
import { NavItem } from "./nav-item";

/// Drive ナビ。挙動:
/// - 親「ドライブ」クリック → /drive へ遷移し、アコーディオンを開く。
/// - 末尾シェブロン → 遷移せず開閉だけをトグル（aria-expanded/controls 付き）。
/// - レール（collapsed）時はアイコンのみの単一リンク（tooltip でラベル補完）。
export function SidebarDriveAccordion({
  collapsed,
  onNavigate,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const pathname = usePathname();
  const driveActive = isActivePath(DRIVE_ROOT, pathname);
  const [open, setOpen] = React.useState(driveActive);
  const listId = React.useId();

  // Drive 配下へ遷移したら自動で開く。
  React.useEffect(() => {
    if (driveActive) setOpen(true);
  }, [driveActive]);

  if (collapsed) {
    return (
      <NavItem
        icon={DRIVE_ICON}
        label="ドライブ"
        href={DRIVE_ROOT}
        active={driveActive}
        collapsed
        onClick={onNavigate}
      />
    );
  }

  return (
    <div>
      <div className="relative flex items-center">
        {/* 親ラベル＝/drive への遷移＋展開 */}
        <Link
          href={DRIVE_ROOT}
          onClick={() => {
            setOpen(true);
            onNavigate?.();
          }}
          aria-current={pathname === DRIVE_ROOT ? "page" : undefined}
          className={cn(
            "group relative flex h-9 flex-1 items-center gap-2.5 rounded-[9px] pl-2.5 pr-9 text-[13.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
            driveActive
              ? "bg-sidebar-accent font-medium text-sidebar-foreground"
              : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground",
          )}
        >
          <DRIVE_ICON
            className={cn("size-[18px] shrink-0", driveActive ? "text-sidebar-foreground" : "text-sidebar-foreground/55")}
            aria-hidden
          />
          <span className="flex-1 truncate text-left">ドライブ</span>
        </Link>
        {/* シェブロン＝開閉のみ */}
        <button
          type="button"
          onClick={() => setOpen((p) => !p)}
          aria-expanded={open}
          aria-controls={listId}
          aria-label={open ? "ドライブを閉じる" : "ドライブを開く"}
          className="absolute right-1 flex size-7 items-center justify-center rounded-md text-sidebar-foreground/60 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          <ChevronRight
            className={cn("size-4 transition-transform duration-150", open && "rotate-90")}
            aria-hidden
          />
        </button>
      </div>

      {/* grid-rows 0fr→1fr で高さアニメーション（中身を measure 不要） */}
      <div
        id={listId}
        role="group"
        aria-label="ドライブ"
        className={cn(
          "grid transition-[grid-template-rows] duration-200 ease-[var(--ease-standard)]",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        {/* 閉じている間は inert でタブ順序とポインタから除外する（a11y） */}
        <div className="overflow-hidden" inert={!open}>
          <ul className="mt-0.5 flex flex-col gap-0.5 py-0.5">
            {DRIVE_CHILDREN.map((child, i) => (
              <li key={child.key}>
                <NavItem
                  icon={child.icon}
                  label={child.label}
                  href={child.href}
                  active={
                    child.href === DRIVE_ROOT
                      ? pathname === DRIVE_ROOT
                      : isActivePath(child.href, pathname)
                  }
                  depth={1}
                  seasonIndex={i}
                  onClick={onNavigate}
                />
              </li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
