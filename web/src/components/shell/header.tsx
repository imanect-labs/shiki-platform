"use client";

import { Menu } from "lucide-react";
import { usePathname } from "next/navigation";

import { resolvePageIcon, resolvePageTitle } from "@/lib/nav-config";
import { useSidebar } from "./sidebar/sidebar-context";

/// シェル上部のバー。モバイルではハンバーガでドロワを開く。
/// 既定で現在地（ページタイトル）を表示し、`children` で差し替えもできる。
export function Header({ children }: { children?: React.ReactNode }) {
  const { setMobileOpen } = useSidebar();
  const pathname = usePathname();
  const PageIcon = resolvePageIcon(pathname);
  const title = resolvePageTitle(pathname);

  return (
    <header className="flex h-14 shrink-0 items-center gap-2.5 bg-background/80 px-3 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:px-6">
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
          <span
            className="flex size-8 items-center justify-center text-foreground/70"
            role="img"
            aria-label={title}
            title={title}
          >
            <PageIcon className="size-[19px]" strokeWidth={2} aria-hidden />
          </span>
        )}
      </div>
    </header>
  );
}
