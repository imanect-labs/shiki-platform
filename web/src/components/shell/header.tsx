"use client";

import { usePathname } from "next/navigation";

import { cn } from "@/lib/utils";
import { resolvePageIcon, resolvePageTitle } from "@/lib/nav-config";
import { BrandMark } from "./brand-logo";
import { useSidebar } from "./sidebar/sidebar-context";
import {
  ChromeActions,
  ChromeBackButton,
  ChromeBar,
  usePageChromeValue,
} from "./page-header-context";

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

/// スマホの浮いた操作群の「台」。PC の浮いたサイドバー（角丸・沈んだ面・淡い影）と同じ素材で、
/// 角は丸めすぎない（丸いピルにしない）。
export const CHROME_TRAY = cn(
  "pointer-events-auto flex h-12 items-center rounded-[14px] border border-sidebar-border px-0.5",
  "bg-sidebar/95 shadow-[var(--elevation-md)] backdrop-blur supports-[backdrop-filter]:bg-sidebar/80",
);

/// スマホ左上（メニュー・戻る）の並び。台（枠・面・影）は敷かず、アイコンだけを浮かせる。
/// 各ボタンの丸い下地は背景色と同じ半透明＋ぼかしで、何も無い上では見えず、本文が下を
/// 流れたときだけアイコンを読めるようにする。
export const CHROME_BARE = "pointer-events-auto flex h-12 items-center gap-1";
export const CHROME_BARE_BUTTON =
  "rounded-full bg-background/70 backdrop-blur-md max-md:hover:bg-sidebar-accent";

/// ドロワ（ナビゲーション）を開くボタン。顔はロゴの四つの点（`BrandMark`）。
export function MenuMarkButton({
  onClick,
  label = "メニューを開く",
  className,
}: {
  onClick: () => void;
  label?: string;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className={cn(
        "group flex size-11 items-center justify-center rounded-[10px] text-foreground outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94]",
        className,
      )}
    >
      <BrandMark className="size-6 transition-transform duration-[var(--duration-normal)] ease-[var(--ease-standard)] group-hover:scale-110" />
    </button>
  );
}

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
      {/* スクロールした本文がボタンの間でくっきり重ならないよう、上端を背景色でぼかす。本文を
          ボタンの下まで流すページ（会話）は、潜った本文がボタンの高さまで薄く透けて見えるよう
          浅く・淡くする。それ以外は本文の開始位置（4rem）で消える。 */}
      <div
        aria-hidden
        className={cn(
          "absolute inset-x-0 top-0 bg-gradient-to-b to-transparent",
          spec?.flowUnder
            ? "h-[calc(4rem+env(safe-area-inset-top))] from-background/90 via-background/50"
            : "h-[calc(4rem+env(safe-area-inset-top))] from-background from-30%",
        )}
      />

      <div className="relative flex items-start justify-between gap-2 px-2.5 pt-[max(0.5rem,env(safe-area-inset-top))]">
        <div className={CHROME_BARE}>
          <MenuMarkButton onClick={() => setMobileOpen(true)} className={CHROME_BARE_BUTTON} />
          {spec?.back ? <ChromeBackButton back={spec.back} className={CHROME_BARE_BUTTON} /> : null}
        </div>
        {hasRight && spec ? (
          <div className={cn(CHROME_TRAY, spec.primary && "pl-1")}>
            <ChromeActions spec={spec} divided />
          </div>
        ) : null}
      </div>
    </div>
  );
}
