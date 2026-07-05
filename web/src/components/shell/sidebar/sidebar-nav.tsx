"use client";

import { useRouter, usePathname } from "next/navigation";
import { FileSearch, PenSquare, Search } from "lucide-react";

import { NavItem } from "./nav-item";
import { SidebarDriveAccordion } from "./sidebar-drive-accordion";
import { useSidebar } from "./sidebar-context";

/// サイドバー上部: 検索（/ ヒント）＋ 新しいチャット ＋ ドライブ（アコーディオン）。
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
        <NavItem
          icon={FileSearch}
          label="文書検索"
          collapsed={collapsed}
          active={pathname === "/search"}
          onClick={() => {
            router.push("/search");
            onNavigate?.();
          }}
        />
        <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />
      </nav>
    </div>
  );
}
