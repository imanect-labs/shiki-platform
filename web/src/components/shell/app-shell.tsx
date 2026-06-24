"use client";

import type { ReactNode } from "react";

import { SidebarProvider, useSidebar } from "./sidebar/sidebar-context";
import { Sidebar } from "./sidebar/sidebar";
import { MobileSidebar } from "./mobile-sidebar";
import { SidebarSearch } from "./sidebar/sidebar-search";
import { Header } from "./header";

/// 検索パレットの単一インスタンス（⌘K / "/" リスナーもここに 1 つだけ存在させる）。
/// デスクトップ/モバイル双方のサイドバーが同じ context 状態で開く。
function ShellSearch() {
  const { searchOpen, setSearchOpen } = useSidebar();
  return <SidebarSearch open={searchOpen} onOpenChange={setSearchOpen} />;
}

/// 認証済みエリアの共通シェル。
/// 左サイドバー（デスクトップ）＋ モバイルドロワ ＋ ヘッダ ＋ メインコンテンツ。
export function AppShell({ children }: { children: ReactNode }) {
  return (
    <SidebarProvider>
      <div className="flex h-dvh w-full overflow-hidden bg-background">
        <Sidebar />
        <MobileSidebar />
        <div className="flex min-w-0 flex-1 flex-col">
          <Header />
          <main className="min-h-0 flex-1 overflow-y-auto">{children}</main>
        </div>
        <ShellSearch />
      </div>
    </SidebarProvider>
  );
}
