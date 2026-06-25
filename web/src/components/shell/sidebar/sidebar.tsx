"use client";

import * as React from "react";
import Link from "next/link";
import { ChevronDown, PanelLeft } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useSidebar } from "./sidebar-context";
import { SidebarNav } from "./sidebar-nav";
import { SidebarChatHistory } from "./sidebar-chat-history";
import { SidebarAccount } from "./sidebar-account";
import { SidebarResizer } from "./sidebar-resizer";

/// セクション見出し（参照デザインの小さな大文字ラベル）。
function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-7 items-center px-2.5 pt-1">
      <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-sidebar-foreground/45">
        {children}
      </span>
    </div>
  );
}

/// サイドバー内部レイアウト（デスクトップ aside / モバイルドロワで共用）。
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
      {/* ヘッダ: ワードマーク＋折りたたみトグル（装飾アイコンは置かない） */}
      <div
        className={cn(
          "flex h-14 shrink-0 items-center px-3",
          collapsed ? "justify-center" : "gap-1.5",
        )}
      >
        {!collapsed ? (
          <>
            <Link
              href="/"
              onClick={onNavigate}
              className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              aria-label="ホームへ"
            >
              <span className="text-[17px] font-bold tracking-[-0.02em] text-foreground">Shiki</span>
            </Link>
            <ChevronDown className="size-4 text-sidebar-foreground/45" aria-hidden />
            {showCollapseToggle ? (
              <button
                type="button"
                onClick={toggleCollapsed}
                aria-label="サイドバーを折りたたむ"
                aria-expanded
                className="ml-auto flex size-7 items-center justify-center rounded-md text-sidebar-foreground/55 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              >
                <PanelLeft className="size-[18px]" aria-hidden />
              </button>
            ) : null}
          </>
        ) : showCollapseToggle ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={toggleCollapsed}
                aria-label="サイドバーを開く"
                aria-expanded={false}
                className="flex size-9 items-center justify-center rounded-[9px] text-sidebar-foreground/55 outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              >
                <PanelLeft className="size-[18px]" aria-hidden />
              </button>
            </TooltipTrigger>
            <TooltipContent side="right">サイドバーを開く</TooltipContent>
          </Tooltip>
        ) : null}
      </div>

      {/* 上部固定ナビ（検索＋新しいチャット＋ドライブ） */}
      <div className="shrink-0">
        <SidebarNav collapsed={collapsed} onNavigate={onNavigate} />
      </div>

      {/* 中段: チャット履歴 */}
      {!collapsed ? <SectionLabel>チャット履歴</SectionLabel> : <div className="h-2" />}
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
      {!collapsed ? <SidebarResizer targetRef={asideRef} /> : null}
    </aside>
  );
}
