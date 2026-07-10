"use client";

import { useRouter, usePathname } from "next/navigation";
import { PenSquare, Search } from "lucide-react";

import { APPS_NAV, SKILLS_NAV, WORKFLOWS_NAV, isActivePath } from "@/lib/nav-config";
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
        <SidebarDriveAccordion collapsed={collapsed} onNavigate={onNavigate} />
        {/* スキル / ミニアプリ（Phase 6） */}
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
        {/* ワークフロー（Phase 10） */}
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
