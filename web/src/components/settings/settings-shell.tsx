"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft } from "lucide-react";

import { cn } from "@/lib/utils";
import { ConsoleCrumbContext } from "@/components/admin/admin-page";
import { ProductMenu } from "@/components/admin/admin-shell";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { ThemeToggle } from "@/components/ui/theme-toggle";
import { CHROME_TRAY, MenuMarkButton } from "@/components/shell/header";
import { SETTINGS_SECTION, settingsItemFor } from "./settings-nav";

/// 「shiki に戻る」リンク（PC の左列先頭・スマホのドロワ下部）。
function BackLink({ onClick, className }: { onClick?: () => void; className?: string }) {
  return (
    <Link
      href="/"
      onClick={onClick}
      className={cn(
        "flex items-center gap-2 rounded-md px-2.5 text-[13px] text-muted-foreground outline-none transition-colors hover:bg-sidebar-accent/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        className,
      )}
    >
      <ArrowLeft className="size-4" aria-hidden />
      shiki に戻る
    </Link>
  );
}

/// ユーザー設定のシェル。管理コンソール（AdminShell）と同じ並び（左にメニュー・上にパンくず・
/// 本文は AdminPage）で、セクションが 1 つなのでアイコンレールは持たない。
export function SettingsShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const hit = settingsItemFor(pathname);
  const [mobileOpen, setMobileOpen] = React.useState(false);
  const close = () => setMobileOpen(false);
  const crumb = hit ? `設定・${hit.group}` : "設定";

  return (
    <div className="flex h-dvh w-full overflow-hidden bg-background text-[13.5px]">
      <nav
        aria-label="設定メニュー"
        className="hidden w-60 shrink-0 flex-col border-r border-sidebar-border bg-sidebar md:flex"
      >
        <div className="flex h-12 shrink-0 items-center border-b border-sidebar-border px-2">
          <BackLink className="h-8" />
        </div>
        <div className="px-4 pb-1 pt-4 text-base font-semibold tracking-tight">設定</div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain">
          <ProductMenu section={SETTINGS_SECTION} pathname={pathname} />
        </div>
      </nav>
      <div className="relative flex min-w-0 flex-1 flex-col">
        <header className="hidden h-12 shrink-0 items-center gap-2 border-b px-4 md:flex">
          <nav aria-label="パンくず" className="flex min-w-0 items-center gap-2 text-[13px]">
            <span className={cn("truncate", hit ? "text-muted-foreground" : "font-medium")}>設定</span>
            {hit ? (
              <>
                <span aria-hidden className="text-muted-foreground/50">
                  /
                </span>
                <span className="truncate text-muted-foreground">{hit.group}</span>
                <span aria-hidden className="text-muted-foreground/50">
                  /
                </span>
                <span className="truncate">{hit.item.label}</span>
              </>
            ) : null}
          </nav>
          <span className="flex-1" />
          <ThemeToggle />
        </header>
        <div className="pointer-events-none absolute inset-x-0 top-0 z-30 md:hidden">
          <div className="flex items-start justify-between gap-2 px-2.5 pt-[max(0.5rem,env(safe-area-inset-top))]">
            <div className={CHROME_TRAY}>
              <MenuMarkButton onClick={() => setMobileOpen(true)} label="設定メニューを開く" />
            </div>
          </div>
        </div>
        <main className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain max-md:pt-[calc(4rem+env(safe-area-inset-top))]">
          <ConsoleCrumbContext.Provider value={crumb}>{children}</ConsoleCrumbContext.Provider>
        </main>
      </div>
      <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
        <SheetContent
          side="left"
          className="max-w-72 bg-sidebar pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]"
        >
          <SheetTitle className="border-b border-sidebar-border px-4 py-3.5 text-sm font-semibold">
            設定
          </SheetTitle>
          <nav aria-label="設定メニュー" className="flex-1 overflow-y-auto overscroll-y-contain">
            <ProductMenu section={SETTINGS_SECTION} pathname={pathname} onNavigate={close} />
          </nav>
          <div className="flex flex-col gap-1 border-t border-sidebar-border px-2 py-2">
            <BackLink onClick={close} className="min-h-11 text-sm" />
            <div className="flex items-center justify-between gap-2 px-2 py-1">
              <span className="text-xs text-muted-foreground">テーマ</span>
              <ThemeToggle className="[&_button]:size-10" />
            </div>
          </div>
        </SheetContent>
      </Sheet>
    </div>
  );
}
