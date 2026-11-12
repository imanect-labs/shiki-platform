"use client";

/// ドライブの区画タブ（ホーム／最近使った／お気に入り／共有済み／ゴミ箱）。
///
/// サイドバー／ドロワにはドライブの子メニューを出さないため、PC・スマホとも本文の上端に
/// 横並びのタブとして常に見せる（OneDrive アプリ風: アイコンの下にラベル、表示中は下線）。
/// スマホは幅いっぱいに等分し、足りない端末では横スクロールして表示中のタブを見える位置へ
/// 寄せる。PC は内容幅で左に寄せる。フォルダの中は戻る＋パンくずが現在地を担うので出さない。

import * as React from "react";
import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";

import { DRIVE_CHILDREN, DRIVE_ROOT, isActivePath } from "@/lib/nav-config";
import { cn } from "@/lib/utils";

export function DriveTabs() {
  return (
    // useSearchParams（フォルダ判定）は Suspense 境界が要る。待つ間はフォルダ外として出す。
    <React.Suspense fallback={<DriveTabsBar />}>
      <FolderAwareTabs />
    </React.Suspense>
  );
}

function FolderAwareTabs() {
  // DriveBrowser と同じ判定（`?folder=` の空値はルート扱い）。
  const inFolder = Boolean(useSearchParams().get("folder"));
  return inFolder ? null : <DriveTabsBar />;
}

function DriveTabsBar() {
  const pathname = usePathname();
  const listRef = React.useRef<HTMLUListElement>(null);

  // 表示中のタブが画面外（横スクロールの先）にあれば寄せる。
  React.useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[aria-current="page"]')
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [pathname]);

  return (
    <nav aria-label="ドライブ" className="mx-auto mb-3 w-full max-w-5xl px-4 md:mb-0 md:px-8 md:pt-5">
      <ul
        ref={listRef}
        className="-mx-4 flex overflow-x-auto overscroll-x-contain px-2 [scrollbar-width:none] md:mx-0 md:gap-1 md:border-b md:border-border md:px-0 [&::-webkit-scrollbar]:hidden"
      >
        {DRIVE_CHILDREN.map((c) => {
          const active = c.href === DRIVE_ROOT ? pathname === DRIVE_ROOT : isActivePath(c.href, pathname);
          const Icon = c.icon;
          return (
            <li key={c.key} className="flex-1 md:flex-none">
              <Link
                href={c.href}
                aria-current={active ? "page" : undefined}
                data-testid={`drive-tab-${c.key}`}
                className={cn(
                  "relative flex min-w-[4.5rem] flex-col items-center gap-1 rounded-lg px-2 pb-2.5 pt-1.5 text-[12px] outline-none md:min-w-[5.5rem] md:px-3 md:hover:bg-accent/50 md:hover:text-foreground",
                  // 横スクロールの ul は縦もはみ出しを切るため、フォーカスリングは内側に描く。
                  "transition-colors duration-[var(--duration-fast)] focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring active:bg-accent/60",
                  active ? "font-semibold text-foreground" : "font-medium text-muted-foreground",
                )}
              >
                <Icon
                  className={cn("size-[22px]", active ? "text-primary" : "text-muted-foreground/80")}
                  aria-hidden
                />
                <span className="whitespace-nowrap">{c.label}</span>
                {active ? (
                  <span aria-hidden className="absolute inset-x-3 bottom-0 h-[3px] rounded-full bg-primary" />
                ) : null}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
