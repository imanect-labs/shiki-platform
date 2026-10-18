"use client";

import * as React from "react";
import type { ReactNode } from "react";
import { usePathname } from "next/navigation";

import { cn } from "@/lib/utils";
import { isEditorRoute } from "@/lib/nav-config";
import { SidebarProvider, useSidebar } from "./sidebar/sidebar-context";
import { Sidebar } from "./sidebar/sidebar";
import { MobileSidebar } from "./mobile-sidebar";
import { SidebarSearch } from "./sidebar/sidebar-search";
import { Header, MobileChrome } from "./header";
import { PageHeaderProvider, usePageChromeValue } from "./page-header-context";

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

/// PC の上部バーの表示制御。没入エディタでは既定の現在地バー（📁 現在地）を出さず縦の作業
/// 領域を最大化する。ただしエディタが usePageChrome でクロームを登録している場合はそれを唯一の
/// バーとして表示する。スマホはバーを出さず `MobileChrome` が角に操作を浮かせる。
function ShellHeader() {
  const pathname = usePathname();
  const spec = usePageChromeValue();
  if (isEditorRoute(pathname) && !spec) return null;
  return <Header />;
}

/// 本文のスクロール領域。スマホでは浮いたボタンの分だけ上端を空ける（会話のように本文を
/// ボタンの下まで流すページは `flowUnder` で外す）。
function ShellMain({ children }: { children: ReactNode }) {
  const spec = usePageChromeValue();
  return (
    <main
      className={cn(
        "min-h-0 flex-1 overflow-y-auto overscroll-y-contain",
        !spec?.flowUnder && "max-md:pt-[calc(4rem+env(safe-area-inset-top))]",
      )}
    >
      {children}
    </main>
  );
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
          <div className="relative flex min-w-0 flex-1 flex-col">
            <ShellHeader />
            <MobileChrome />
            <ShellMain>{children}</ShellMain>
          </div>
          <ShellSearch />
        </div>
      </PageHeaderProvider>
    </SidebarProvider>
  );
}
