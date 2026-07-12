"use client";

import { useRouter, usePathname } from "next/navigation";
import { PenSquare, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { APPS_NAV, SKILLS_NAV, WORKFLOWS_NAV, isActivePath } from "@/lib/nav-config";
import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { NavItem } from "./nav-item";
import { SidebarDriveAccordion } from "./sidebar-drive-accordion";
import { useSidebar } from "./sidebar-context";

/// サイドバー上部: 新規チャット（主役 CTA）＋ 検索（検索ボックス風）＋ ナビ（ドライブ/スキル/アプリ/ワークフロー）。
/// 検索ダイアログ本体はシェルで単一管理し、ここは開くトリガーのみ（多重マウント回避）。
export function SidebarNav({
  collapsed,
  onNavigate,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const router = useRouter();
  const pathname = usePathname();
  const { setSearchOpen } = useSidebar();

  const startNewChat = () => {
    router.push("/");
    onNavigate?.();
  };

  return (
    <div className="flex flex-col gap-2 px-2.5">
      {/* 主役 CTA: 新しいチャット（白の浮き上がるボタン＋季節色のペン・押下フィードバック）。
          重い navy 塗りは避け、カード面＋境界＋淡い影で「押せる主役」を軽やかに示す。 */}
      {collapsed ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={startNewChat}
              aria-label="新しいチャット"
              className="mx-auto flex size-9 items-center justify-center rounded-[10px] border border-sidebar-border bg-card text-sidebar-foreground shadow-sm outline-none transition-[transform,box-shadow] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:shadow-md focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-95"
            >
              <PenSquare
                className="size-[18px]"
                style={{ color: seasonVar(currentSeasonIndex()) }}
                aria-hidden
              />
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">新しいチャット</TooltipContent>
        </Tooltip>
      ) : (
        <button
          type="button"
          onClick={startNewChat}
          className="flex h-9 w-full items-center gap-2 rounded-[10px] border border-sidebar-border bg-card px-3 text-[13.5px] font-medium text-sidebar-foreground shadow-sm outline-none transition-[transform,background-color,box-shadow] duration-[var(--duration-fast)] ease-[var(--ease-standard)] hover:bg-sidebar-accent/40 hover:shadow-md focus-visible:ring-2 focus-visible:ring-sidebar-ring active:scale-[0.98]"
        >
          <PenSquare
            className="size-[18px] shrink-0"
            style={{ color: seasonVar(currentSeasonIndex()) }}
            aria-hidden
          />
          新しいチャット
        </button>
      )}

      {/* 検索: 検索ボックス風のトリガ（実体はシェルのパレット）。 */}
      {collapsed ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              onClick={() => setSearchOpen(true)}
              aria-label="検索"
              className="mx-auto flex size-9 items-center justify-center rounded-[10px] text-sidebar-foreground/60 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
            >
              <Search className="size-[18px]" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">検索</TooltipContent>
        </Tooltip>
      ) : (
        <button
          type="button"
          onClick={() => setSearchOpen(true)}
          className="group flex h-9 w-full items-center gap-2.5 rounded-[10px] border border-sidebar-border bg-sidebar-accent/40 px-2.5 text-left outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          <Search className="size-4 shrink-0 text-sidebar-foreground/45" aria-hidden />
          <span className="flex-1 text-[13px] text-sidebar-foreground/45">検索</span>
          <kbd className="rounded border border-sidebar-border bg-sidebar px-1.5 py-0.5 text-[10px] font-medium leading-none text-sidebar-foreground/45">
            ⌘K
          </kbd>
        </button>
      )}

      {/* ナビ（検索/新規チャットとは間を空けて視覚分離）。 */}
      <nav
        aria-label="メインナビゲーション"
        className={cn("flex flex-col gap-0.5", collapsed ? "pt-1" : "pt-1.5")}
      >
        <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />
        <NavItem
          icon={SKILLS_NAV.icon}
          label={SKILLS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(SKILLS_NAV.href, pathname)}
          onClick={() => {
            router.push(SKILLS_NAV.href);
            onNavigate?.();
          }}
        />
        <NavItem
          icon={APPS_NAV.icon}
          label={APPS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(APPS_NAV.href, pathname)}
          onClick={() => {
            router.push(APPS_NAV.href);
            onNavigate?.();
          }}
        />
        <NavItem
          icon={WORKFLOWS_NAV.icon}
          label={WORKFLOWS_NAV.label}
          collapsed={collapsed}
          active={isActivePath(WORKFLOWS_NAV.href, pathname)}
          onClick={() => {
            router.push(WORKFLOWS_NAV.href);
            onNavigate?.();
          }}
        />
      </nav>
    </div>
  );
}
