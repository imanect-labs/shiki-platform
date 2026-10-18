"use client";

import { usePathname } from "next/navigation";

import { cn } from "@/lib/utils";
import { resolvePageIcon, resolvePageTitle } from "@/lib/nav-config";
import { useSidebar } from "./sidebar/sidebar-context";
import { ChromeActions, ChromeBackButton, ChromeBar, usePageChromeValue } from "./page-header-context";

/// PC（md 以上）の上部バー。ページが `usePageChrome` で登録した spec を描き、
/// 無ければルート由来の現在地（アイコン＋タイトル）を既定表示する。
/// 下端は破線ルール（shiki-dash-bottom）で本文とやわらかく分ける。
/// スマホではタイトルバーを出さない（`MobileChrome` が角に操作を浮かせる）。
export function Header() {
  const pathname = usePathname();
  const spec = usePageChromeValue();
  const PageIcon = resolvePageIcon(pathname);
  const title = resolvePageTitle(pathname);

  return (
    <header className="shiki-dash-bottom hidden h-14 shrink-0 items-center gap-2.5 bg-background/80 px-6 backdrop-blur supports-[backdrop-filter]:bg-background/60 md:flex">
      {spec ? (
        <ChromeBar spec={spec} />
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

const CAPSULE = cn(
  "pointer-events-auto flex h-[46px] items-center rounded-full border border-foreground/[0.07] px-px",
  "bg-background/90 shadow-[0_4px_14px_-6px_rgb(0_0_0/0.18)] backdrop-blur supports-[backdrop-filter]:bg-background/75",
);

/// スマホ（md 未満）の浮いた操作群。左上にメニュー（＋戻る）、右上にページの操作を置く。
/// タイトルバーの代わりなので、どの画面でもドロワへの導線が必ず残る（没入エディタ・
/// 読み込み中・notfound でも消えない）。
export function MobileChrome() {
  const { setMobileOpen } = useSidebar();
  const spec = usePageChromeValue();
  const hasRight = Boolean(
    spec && (spec.primary || spec.actions || spec.assistant || (spec.menu && spec.menu.length > 0)),
  );

  return (
    <div className="pointer-events-none absolute inset-x-0 top-0 z-30 md:hidden">
      {spec?.flowUnder ? (
        <div
          aria-hidden
          className="absolute inset-x-0 top-0 h-[calc(5.25rem+env(safe-area-inset-top))] bg-gradient-to-b from-background from-30% to-transparent"
        />
      ) : null}
      <div className="relative flex items-start justify-between gap-2 px-2.5 pt-[max(0.625rem,env(safe-area-inset-top))]">
        <div className={CAPSULE}>
          <button
            type="button"
            onClick={() => setMobileOpen(true)}
            aria-label="メニューを開く"
            className="flex size-11 items-center justify-center rounded-full text-foreground/75 outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94]"
          >
            <svg
              viewBox="0 0 24 24"
              className="size-5"
              fill="none"
              stroke="currentColor"
              strokeWidth={2}
              strokeLinecap="round"
              aria-hidden
            >
              <line x1="4" x2="20" y1="9" y2="9" />
              <line x1="4" x2="14" y1="15" y2="15" />
            </svg>
          </button>
          {spec?.back ? <ChromeBackButton back={spec.back} /> : null}
        </div>
        {hasRight && spec ? (
          <div className={cn(CAPSULE, "gap-0.5", spec.primary && "pl-[5px]")}>
            <ChromeActions spec={spec} />
          </div>
        ) : null}
      </div>
    </div>
  );
}
