"use client";

import { Menu } from "lucide-react";
import { usePathname } from "next/navigation";

import { resolvePageTitle } from "@/lib/nav-config";
import { useSidebar } from "./sidebar/sidebar-context";

/// シェル上部のバー。モバイルではハンバーガでドロワを開く。
/// 既定で現在地（ページタイトル）を表示し、`children` で差し替えもできる。
export function Header({ children }: { children?: React.ReactNode }) {
  const { setMobileOpen } = useSidebar();
  const pathname = usePathname();

  return (
    <header className="flex h-14 shrink-0 items-center gap-2.5 border-b border-border bg-background/80 px-3 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:px-6">
      <button
        type="button"
        onClick={() => setMobileOpen(true)}
        aria-label="メニューを開く"
        className="flex size-9 items-center justify-center rounded-md text-foreground/70 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:hidden"
      >
        <Menu className="size-5" aria-hidden />
      </button>
      <div className="flex min-w-0 flex-1 items-center">
        {children ?? (
          <h1 className="truncate text-[15px] font-semibold tracking-[-0.01em] text-foreground">
            {resolvePageTitle(pathname)}
          </h1>
        )}
      </div>
    </header>
  );
}
