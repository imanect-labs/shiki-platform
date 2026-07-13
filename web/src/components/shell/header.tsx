"use client";

import { Menu } from "lucide-react";
import { usePathname } from "next/navigation";

import { resolvePageIcon, resolvePageTitle } from "@/lib/nav-config";
import { useSidebar } from "./sidebar/sidebar-context";
import { usePageHeaderValue } from "./page-header-context";

/// シェル上部の唯一のバー。モバイルではハンバーガでドロワを開く。
/// 各ページが usePageHeader でタイトル/アクションを注入した場合はそれを描画し、
/// 無ければルート由来の現在地（アイコン＋タイトル）を既定表示する。
/// 下端は破線ルール（shiki-dash-bottom）で本文とやわらかく分ける。
export function Header() {
  const { setMobileOpen } = useSidebar();
  const pathname = usePathname();
  const injected = usePageHeaderValue();
  const PageIcon = resolvePageIcon(pathname);
  const title = resolvePageTitle(pathname);

  return (
    <header className="shiki-dash-bottom flex h-14 shrink-0 items-center gap-2.5 bg-background/80 px-3 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:px-6">
      <button
        type="button"
        onClick={() => setMobileOpen(true)}
        aria-label="メニューを開く"
        className="flex size-9 items-center justify-center rounded-md text-foreground/70 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:hidden"
      >
        <Menu className="size-5" aria-hidden />
      </button>
      {injected ? (
        injected
      ) : (
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <span
            className="flex size-8 items-center justify-center text-foreground/70"
            role="img"
            aria-label={title}
            title={title}
          >
            <PageIcon className="size-[19px]" strokeWidth={2} aria-hidden />
          </span>
          <span className="truncate text-sm font-medium text-foreground/80">{title}</span>
        </div>
      )}
    </header>
  );
}
