"use client";

import * as React from "react";
import type { ReactNode } from "react";
import { usePathname } from "next/navigation";

import { isEditorRoute } from "@/lib/nav-config";
import { SidebarProvider, useSidebar } from "./sidebar/sidebar-context";
import { Sidebar } from "./sidebar/sidebar";
import { MobileSidebar } from "./mobile-sidebar";
import { SidebarSearch } from "./sidebar/sidebar-search";
import { Header } from "./header";
import { PageHeaderProvider } from "./page-header-context";

/// 検索パレットの単一インスタンス（⌘K / "/" リスナーもここに 1 つだけ存在させる）。
/// デスクトップ/モバイル双方のサイドバーが同じ context 状態で開く。
function ShellSearch() {
  const { searchOpen, setSearchOpen } = useSidebar();
  return <SidebarSearch open={searchOpen} onOpenChange={setSearchOpen} />;
}

/// 没入エディタに入ったらサイドバーを畳み、離れたら元に戻す。編集中の集中体験のため
/// （human 要望）。ユーザーが編集中に手動で開いた場合はその状態を尊重し、離脱時に
/// 「入った時の状態」へ復元する。手動 pref を恒久的に書き換えない。
function ImmersiveController() {
  const pathname = usePathname();
  const { setRouteImmersive } = useSidebar();
  // 没入エディタのルートにいる間だけ一時折りたたみ（手動 pref は不変・離脱で自動復元）。
  React.useEffect(() => {
    setRouteImmersive(isEditorRoute(pathname));
  }, [pathname, setRouteImmersive]);
  return null;
}

/// 上部バー（Header）を出すかはルート次第。没入エディタでは各エディタが自前の
/// ツールバー/タイトルを持つため、シェルの汎用バーは畳んで縦の作業領域を最大化する。
function ShellHeader() {
  const pathname = usePathname();
  if (isEditorRoute(pathname)) return null;
  return <Header />;
}

/// 認証済みエリアの共通シェル。
/// 左サイドバー（デスクトップ）＋ モバイルドロワ ＋ ヘッダ ＋ メインコンテンツ。
export function AppShell({ children }: { children: ReactNode }) {
  return (
    <SidebarProvider>
      <PageHeaderProvider>
        <ImmersiveController />
        <div className="flex h-dvh w-full overflow-hidden bg-background">
          <Sidebar />
          <MobileSidebar />
          <div className="flex min-w-0 flex-1 flex-col">
            <ShellHeader />
            <main className="min-h-0 flex-1 overflow-y-auto">{children}</main>
          </div>
          <ShellSearch />
        </div>
      </PageHeaderProvider>
    </SidebarProvider>
  );
}
