"use client";

import * as React from "react";
import { useRouter, usePathname } from "next/navigation";
import { PenSquare, Search } from "lucide-react";

import { cn } from "@/lib/utils";
import { NavItem } from "./nav-item";
import { SidebarDriveAccordion } from "./sidebar-drive-accordion";
import { SidebarSearch } from "./sidebar-search";

/// サイドバー上部の固定ナビ。新しいチャット / 検索 / ドライブ（アコーディオン）。
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
    <nav aria-label="メインナビゲーション" className="flex flex-col gap-0.5 px-2">
      <NavItem
        icon={PenSquare}
        label="新しいチャット"
        collapsed={collapsed}
        active={pathname === "/"}
        onClick={startNewChat}
        className={cn(!collapsed && "font-semibold")}
      />
      <NavItem
        icon={Search}
        label="検索"
        collapsed={collapsed}
        onClick={() => setSearchOpen(true)}
      />
      <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />

      <SidebarSearch open={searchOpen} onOpenChange={setSearchOpen} />
    </nav>
  );
}
