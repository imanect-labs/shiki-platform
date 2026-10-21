"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft } from "lucide-react";

import { cn, initialsFrom } from "@/lib/utils";
import { useMe } from "@/hooks/use-me";
import { BrandLogo } from "@/components/shell/brand-logo";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { CHROME_TRAY, MenuMarkButton } from "@/components/shell/header";
import { NavItem } from "@/components/shell/sidebar/nav-item";
import { SETTINGS_NAV, settingsItemFor } from "./settings-nav";
import { SettingsCrumbContext, SoonBadge } from "./settings-ui";

/// ロゴ（四つの点＋ワードマーク）。アプリのサイドバーと同じ。ホームへのリンクを兼ねる。
function Wordmark({ onNavigate, mobile }: { onNavigate?: () => void; mobile?: boolean }) {
  return (
    <Link
      href="/"
      onClick={onNavigate}
      aria-label="ホームへ"
      className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
    >
      <BrandLogo size={mobile ? "touch" : "md"} />
    </Link>
  );
}

/// メニュー本体（PC の浮いたサイドバー・スマホのドロワで共用）。
function SettingsNavList({ pathname, onNavigate }: { pathname: string; onNavigate?: () => void }) {
  let index = 0;
  return (
    <nav aria-label="設定メニュー" className="flex flex-col gap-4 px-2.5 py-3">
      {SETTINGS_NAV.map((group) => (
        <div key={group.label} className="flex flex-col gap-0.5">
          <div className="px-2.5 pb-1 text-[11.5px] font-medium text-sidebar-foreground/45">{group.label}</div>
          {group.items.map((item) => {
            const seasonIndex = index++;
            const active = pathname === item.href || pathname.startsWith(`${item.href}/`);
            return (
              <NavItem
                key={item.href}
                href={item.href}
                icon={item.icon}
                label={item.label}
                active={active}
                seasonIndex={seasonIndex}
                onClick={onNavigate}
                trailing={item.mock ? <SoonBadge /> : null}
              />
            );
          })}
        </div>
      ))}
    </nav>
  );
}

/// 最下部の自分（設定の持ち主を示すだけ・操作は持たない）。
function AccountFooter() {
  const { data, loading } = useMe();
  if (loading || !data) {
    return (
      <div className="flex items-center gap-3 p-3">
        <Skeleton className="size-8 rounded-full" />
        <div className="flex flex-1 flex-col gap-1.5">
          <Skeleton className="h-3 w-24" />
          <Skeleton className="h-2.5 w-32" />
        </div>
      </div>
    );
  }
  const name = data.email ? data.email.slice(0, data.email.indexOf("@")) || data.email : data.id;
  return (
    <div className="flex items-center gap-3 px-4 py-3">
      <Avatar>
        <AvatarFallback>{initialsFrom(data.email ?? data.id)}</AvatarFallback>
      </Avatar>
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-sm font-medium">{name}</span>
        <span className="truncate text-xs text-sidebar-foreground/50">{data.email ?? data.org}</span>
      </span>
    </div>
  );
}

function SidebarBody({ pathname, onNavigate, mobile }: { pathname: string; onNavigate?: () => void; mobile?: boolean }) {
  return (
    <div className="flex h-full flex-col bg-sidebar text-sidebar-foreground">
      <div className={cn("flex h-14 shrink-0 items-center gap-2.5", mobile ? "pl-4 pr-2" : "px-3")}>
        <Wordmark onNavigate={onNavigate} mobile={mobile} />
        <span aria-hidden className="shiki-dash-y h-4 w-1.5" />
        <span className="text-[13.5px] font-medium text-sidebar-foreground/70">設定</span>
      </div>
      <div className="shrink-0 px-2.5">
        <NavItem href="/" icon={ArrowLeft} label="ホームに戻る" onClick={onNavigate} />
      </div>
      <div className="shrink-0 px-2.5 pt-3" aria-hidden>
        <div className="rule-soft shiki-dash-x h-1.5" />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain scrollbar-subtle">
        <SettingsNavList pathname={pathname} onNavigate={onNavigate} />
      </div>
      <div className="shiki-dash-top shrink-0 pb-[env(safe-area-inset-bottom)]">
        <AccountFooter />
      </div>
    </div>
  );
}

/// ユーザー設定のシェル。アプリ本体と同じ素材（浮いたサイドバー・破線で分けた上部バー）で、
/// 左にメニュー・上に現在地・本文は SettingsPage。
export function SettingsShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const hit = settingsItemFor(pathname);
  const [mobileOpen, setMobileOpen] = React.useState(false);
  const close = () => setMobileOpen(false);
  const crumb = hit ? `設定・${hit.group}` : "設定";
  const PageIcon = hit?.item.icon;

  return (
    <div className="flex h-dvh w-full overflow-hidden bg-background text-[13.5px]">
      <aside className="relative m-2 hidden h-[calc(100dvh-1rem)] w-60 shrink-0 md:block">
        <div className="h-full overflow-hidden rounded-xl border border-sidebar-border bg-sidebar shadow-sm">
          <SidebarBody pathname={pathname} />
        </div>
      </aside>
      <div className="relative flex min-w-0 flex-1 flex-col">
        <header className="shiki-dash-bottom hidden h-14 shrink-0 items-center gap-2.5 bg-background/80 px-6 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:flex">
          {PageIcon ? (
            <span className="flex size-8 items-center justify-center text-foreground/70" aria-hidden>
              <PageIcon className="size-[19px]" strokeWidth={2} />
            </span>
          ) : null}
          <nav aria-label="現在地" className="flex min-w-0 items-center gap-2 text-sm">
            <span className={cn("truncate", hit ? "text-muted-foreground" : "font-medium text-foreground/80")}>
              設定
            </span>
            {hit ? (
              <>
                <span aria-hidden className="text-muted-foreground/50">
                  /
                </span>
                <span className="truncate font-medium text-foreground/80">{hit.item.label}</span>
              </>
            ) : null}
          </nav>
        </header>
        <div className="pointer-events-none absolute inset-x-0 top-0 z-30 md:hidden">
          <div className="flex items-start justify-between gap-2 px-2.5 pt-[max(0.5rem,env(safe-area-inset-top))]">
            <div className={CHROME_TRAY}>
              <MenuMarkButton onClick={() => setMobileOpen(true)} label="設定メニューを開く" />
            </div>
          </div>
        </div>
        <main className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain max-md:pt-[calc(4rem+env(safe-area-inset-top))]">
          <SettingsCrumbContext.Provider value={crumb}>{children}</SettingsCrumbContext.Provider>
        </main>
      </div>
      <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
        <SheetContent side="left" className="max-w-72 p-0 pt-[env(safe-area-inset-top)]">
          <SheetTitle className="sr-only">設定メニュー</SheetTitle>
          <SidebarBody pathname={pathname} onNavigate={close} mobile />
        </SheetContent>
      </Sheet>
    </div>
  );
}
