"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft, ShieldAlert } from "lucide-react";

import { cn } from "@/lib/utils";
import { useMe } from "@/hooks/use-me";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { ThemeToggle } from "@/components/ui/theme-toggle";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CHROME_TRAY, MenuMarkButton } from "@/components/shell/header";
import { ADMIN_SECTIONS, menuItemFor, sectionFor, type AdminSection } from "./admin-nav";
import { MockBadge } from "./mock-banner";

/// スマホの本文先頭に出すパンくず（「a-corp・アクセス管理」）。AdminPage が見出しの上に出す。
/// スマホにはタイトルバーが無いので、現在地はここで示す。
const AdminCrumbContext = React.createContext<string | null>(null);

export function useAdminCrumb(): string | null {
  return React.useContext(AdminCrumbContext);
}

/// 左端のアイコンレール（セクション切替）。
function Rail({ active }: { active: AdminSection }) {
  return (
    <nav
      aria-label="管理メニュー"
      className="hidden w-14 shrink-0 flex-col items-center gap-1 border-r border-sidebar-border bg-sidebar py-2.5 md:flex"
    >
      <Tooltip>
        <TooltipTrigger asChild>
          <Link
            href="/"
            aria-label="shiki に戻る"
            className="mb-2 flex size-8 items-center justify-center rounded-lg bg-primary text-[13px] font-bold text-primary-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            四
          </Link>
        </TooltipTrigger>
        <TooltipContent side="right">shiki に戻る</TooltipContent>
      </Tooltip>
      {ADMIN_SECTIONS.map((s, i) => {
        const Icon = s.icon;
        const current = s.key === active.key;
        return (
          <React.Fragment key={s.key}>
            {i === 1 ? <span className="my-1.5 w-6 border-t border-sidebar-border" /> : null}
            <Tooltip>
              <TooltipTrigger asChild>
                <Link
                  href={s.href}
                  aria-label={s.label}
                  aria-current={current ? "page" : undefined}
                  className={cn(
                    "flex size-9 items-center justify-center rounded-lg outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                    current
                      ? "bg-sidebar-accent text-sidebar-accent-foreground"
                      : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-foreground",
                  )}
                >
                  <Icon className="size-[18px]" strokeWidth={1.8} aria-hidden />
                </Link>
              </TooltipTrigger>
              <TooltipContent side="right">{s.label}</TooltipContent>
            </Tooltip>
          </React.Fragment>
        );
      })}
    </nav>
  );
}

/// 2 列目のプロダクトメニュー（セクション内のページ）。
function ProductMenu({
  section,
  pathname,
  onNavigate,
}: {
  section: AdminSection;
  pathname: string;
  onNavigate?: () => void;
}) {
  if (!section.menu) return null;
  return (
    <div className="flex flex-col gap-0.5 px-2 py-2.5">
      {section.menu.map((group) => (
        <div key={group.label} className="flex flex-col gap-0.5">
          <div className="px-2.5 pb-1.5 pt-3 font-mono text-[11px] tracking-wide text-muted-foreground">
            {group.label}
          </div>
          {group.items.map((item) => {
            const current = pathname === item.href || pathname.startsWith(`${item.href}/`);
            return (
              <Link
                key={item.href}
                href={item.href}
                onClick={onNavigate}
                aria-current={current ? "page" : undefined}
                className={cn(
                  "flex items-center justify-between rounded-md px-2.5 py-1.5 text-[13px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring pointer-coarse:min-h-11 pointer-coarse:text-sm",
                  current
                    ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                    : "text-foreground/75 hover:bg-sidebar-accent/60 hover:text-foreground",
                )}
              >
                {item.label}
                {item.mock ? <MockBadge /> : null}
              </Link>
            );
          })}
        </div>
      ))}
    </div>
  );
}

/// モバイル用のナビ（レール＋メニューをドロワにまとめる）。下部にテーマ切替。
function MobileNav({
  open,
  onOpenChange,
  pathname,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  pathname: string;
}) {
  const close = () => onOpenChange(false);
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="left"
        className="max-w-72 bg-sidebar pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]"
      >
        <SheetTitle className="border-b border-sidebar-border px-4 py-3.5 text-sm font-semibold">
          管理コンソール
        </SheetTitle>
        <nav aria-label="管理メニュー" className="flex-1 overflow-y-auto overscroll-y-contain py-2">
          {ADMIN_SECTIONS.map((s) => {
            const Icon = s.icon;
            const current = sectionFor(pathname).key === s.key && !s.menu;
            return (
              <div key={s.key}>
                <Link
                  href={s.href}
                  onClick={close}
                  aria-current={current ? "page" : undefined}
                  className={cn(
                    "flex min-h-10 items-center gap-2.5 px-4 py-2 text-sm font-medium outline-none hover:bg-sidebar-accent/60 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-sidebar-ring pointer-coarse:min-h-11",
                    current && "bg-sidebar-accent text-sidebar-accent-foreground",
                  )}
                >
                  <Icon className="size-4 text-muted-foreground" aria-hidden />
                  {s.label}
                  {s.mock ? <MockBadge /> : null}
                </Link>
                <ProductMenu section={s} pathname={pathname} onNavigate={close} />
              </div>
            );
          })}
        </nav>
        <div className="flex flex-col gap-1 border-t border-sidebar-border px-2 py-2">
          <Link
            href="/"
            onClick={close}
            className="flex min-h-11 items-center gap-2 rounded-md px-2 text-sm text-muted-foreground outline-none hover:bg-sidebar-accent/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
          >
            <ArrowLeft className="size-4" aria-hidden />
            shiki に戻る
          </Link>
          <div className="flex items-center justify-between gap-2 px-2 py-1">
            <span className="text-xs text-muted-foreground">テーマ</span>
            <ThemeToggle className="[&_button]:size-10" />
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}

/// 管理者でない／読み込み中のときの本文。
function Gate({ children }: { children: React.ReactNode }) {
  const { data, loading } = useMe();
  if (loading) {
    return (
      <div className="mx-auto flex max-w-5xl flex-col gap-4 p-6" aria-busy="true">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-28 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }
  if (!data?.is_admin) {
    return (
      <div className="mx-auto max-w-xl p-6 pt-16">
        <EmptyState
          icon={ShieldAlert}
          title="組織の管理者だけが使えます"
          description="管理コンソールを使うには、組織の管理者に権限の付与を依頼してください。"
          action={
            <Button asChild variant="outline" size="sm">
              <Link href="/">shiki に戻る</Link>
            </Button>
          }
        />
      </div>
    );
  }
  return <>{children}</>;
}

/// 管理コンソールのシェル（Supabase 風の 2 段ナビ）。shiki 本体のシェルとは別に持つ。
export function AdminShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const section = sectionFor(pathname);
  const item = menuItemFor(pathname);
  const { data } = useMe();
  const [mobileOpen, setMobileOpen] = React.useState(false);
  const org = data?.org ?? "組織";
  const crumb = section.key === "home" ? org : `${org}・${section.label}`;

  return (
    <div className="flex h-dvh w-full overflow-hidden bg-background text-[13.5px]">
      <Rail active={section} />
      {section.menu ? (
        <nav
          aria-label={section.label}
          className="hidden w-56 shrink-0 flex-col border-r border-sidebar-border bg-sidebar/60 md:flex"
        >
          <div className="flex h-12 shrink-0 items-center border-b border-sidebar-border px-4 text-sm font-semibold">
            {section.label}
          </div>
          <ProductMenu section={section} pathname={pathname} />
        </nav>
      ) : null}
      <div className="relative flex min-w-0 flex-1 flex-col">
        {/* PC: パンくずの上部バー。スマホはタイトルバーを出さず、左上に浮いたメニューボタン
            （右上の操作は各画面の AdminPage が置く）。 */}
        <header className="hidden h-12 shrink-0 items-center gap-2 border-b px-4 md:flex">
          <nav aria-label="パンくず" className="flex min-w-0 items-center gap-2 text-[13px]">
            <span className="truncate font-medium">{data?.org ?? "組織"}</span>
            {section.key !== "home" ? (
              <>
                <span aria-hidden className="text-muted-foreground/50">
                  /
                </span>
                <span className={cn("truncate", item ? "text-muted-foreground" : "")}>
                  {section.label}
                </span>
              </>
            ) : null}
            {item ? (
              <>
                <span aria-hidden className="text-muted-foreground/50">
                  /
                </span>
                <span className="truncate">{item.label}</span>
              </>
            ) : null}
          </nav>
          <span className="flex-1" />
          <ThemeToggle />
        </header>
        <div className="pointer-events-none absolute inset-x-0 top-0 z-30 md:hidden">
          <div className="flex items-start justify-between gap-2 px-2.5 pt-[max(0.5rem,env(safe-area-inset-top))]">
            <div className={CHROME_TRAY}>
              <MenuMarkButton onClick={() => setMobileOpen(true)} label="管理メニューを開く" />
            </div>
          </div>
        </div>
        <main className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain max-md:pt-[calc(4rem+env(safe-area-inset-top))]">
          <AdminCrumbContext.Provider value={crumb}>
            <Gate>{children}</Gate>
          </AdminCrumbContext.Provider>
        </main>
      </div>
      <MobileNav open={mobileOpen} onOpenChange={setMobileOpen} pathname={pathname} />
    </div>
  );
}
