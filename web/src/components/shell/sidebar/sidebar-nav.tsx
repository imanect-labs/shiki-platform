"use client";

import * as React from "react";
import { useRouter, usePathname } from "next/navigation";
import { PenSquare, Search } from "lucide-react";

import { NavItem } from "./nav-item";
import { SidebarDriveAccordion } from "./sidebar-drive-accordion";
import { SidebarSearch } from "./sidebar-search";

/// サイドバー上部: 検索ボックス（/ ヒント）＋ 新しいチャット ＋ ドライブ（アコーディオン）。
export function SidebarNav({
  collapsed,
  onNavigate,
}: {
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const router = useRouter();
  const pathname = usePathname();
  const [searchOpen, setSearchOpen] = React.useState(false);

  const startNewChat = () => {
    router.push("/");
    onNavigate?.();
  };

  return (
    <div className="px-2.5">
      {/* 一次ナビ（検索も他の項目と同じ行スタイル） */}
      <nav aria-label="メインナビゲーション" className="flex flex-col gap-0.5">
        <NavItem
          icon={Search}
          label="検索"
          collapsed={collapsed}
          onClick={() => setSearchOpen(true)}
          trailing={
            !collapsed ? (
              <kbd className="text-[11px] leading-none text-sidebar-foreground/40">/</kbd>
            ) : undefined
          }
        />
        <NavItem
          icon={PenSquare}
          label="新しいチャット"
          collapsed={collapsed}
          active={pathname === "/"}
          onClick={startNewChat}
        />
        <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />
      </nav>

      <SidebarSearch open={searchOpen} onOpenChange={setSearchOpen} />
    </div>
  );
}
