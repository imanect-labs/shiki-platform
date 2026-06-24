"use client";

import * as React from "react";
import Link from "next/link";
import { PanelLeft } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useSidebar } from "./sidebar-context";
import { SidebarNav } from "./sidebar-nav";
import { SidebarChatHistory } from "./sidebar-chat-history";
import { SidebarAccount } from "./sidebar-account";
import { SidebarResizer } from "./sidebar-resizer";

/// ブランドマーク（グラデーションの角丸＋monogram「式」）。
function BrandMark() {
  return (
    <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br from-primary to-[oklch(0.62_0.21_310)] text-sm font-bold text-primary-foreground shadow-sm">
      式
    </span>
  );
}

/// サイドバー内部レイアウト（デスクトップ aside / モバイルドロワで共用）。
/// collapsed=レール、onNavigate=遷移時にドロワを閉じる等のフック。
export function SidebarContent({
  collapsed,
  onNavigate,
  showCollapseToggle = true,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
  showCollapseToggle?: boolean;
}) {
  const { toggleCollapsed } = useSidebar();

  return (
    <div className="flex h-full flex-col bg-sidebar text-sidebar-foreground">
      {/* ヘッダ: ブランド＋折りたたみトグル */}
      <div
        className={cn(
          "flex h-14 shrink-0 items-center gap-2 px-3",
          collapsed ? "justify-center" : "justify-between",
        )}
      >
        {!collapsed ? (
          <Link
            href="/"
            onClick={onNavigate}
            className="flex items-center gap-2 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
            aria-label="ホームへ"
          >
            <BrandMark />
            <span className="text-base font-semibold tracking-tight">shiki</span>
          </Link>
        ) : (
          <Link href="/" onClick={onNavigate} aria-label="ホームへ">
            <BrandMark />
          </Link>
        )}

        {showCollapseToggle ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={toggleCollapsed}
                aria-label={collapsed ? "サイドバーを開く" : "サイドバーを折りたたむ"}
                aria-expanded={!collapsed}
                className={cn(
                  "flex size-8 items-center justify-center rounded-md text-sidebar-foreground/70 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                  collapsed && "absolute right-2 top-3", // レール時はブランド下に重ならないよう配置
                )}
              >
                <PanelLeft className="size-4" aria-hidden />
              </button>
            </TooltipTrigger>
            <TooltipContent side="right">
              {collapsed ? "サイドバーを開く" : "折りたたむ"}
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>

      {/* 上部固定ナビ */}
      <div className="shrink-0 pb-1">
        <SidebarNav collapsed={collapsed} onNavigate={onNavigate} />
      </div>

      {/* 中段: スクロール可能なチャット履歴 */}
      <SidebarChatHistory collapsed={collapsed} />

      {/* 最下部: アカウント */}
      <div className="shrink-0 border-t border-sidebar-border">
        <SidebarAccount collapsed={collapsed} onNavigate={onNavigate} />
      </div>
    </div>
  );
}

/// デスクトップの aside。幅は context 由来、右端にリサイズハンドル。
export function Sidebar() {
  const { collapsed, effectiveWidthPx } = useSidebar();
  const asideRef = React.useRef<HTMLElement | null>(null);

  return (
    <aside
      ref={asideRef}
      style={{ width: effectiveWidthPx }}
      data-collapsed={collapsed}
      className="relative hidden h-dvh shrink-0 border-r border-sidebar-border transition-[width] duration-200 ease-[var(--ease-standard)] md:block"
    >
      <SidebarContent collapsed={collapsed} />
      {/* レール時は幅固定（リサイズ不可） */}
      {!collapsed ? <SidebarResizer targetRef={asideRef} /> : null}
    </aside>
  );
}
