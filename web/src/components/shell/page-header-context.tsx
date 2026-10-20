"use client";

/// ページ クローム（上部の操作群）の単一定義。
///
/// 各ページは `usePageChrome(spec)` で「タイトル・戻る・主操作・アイコン操作・アシスタント・⋯」を
/// **構造として**渡す。シェルは同じ spec から 2 通りに描く:
///
/// - PC（md 以上）: 上部の 1 本のバー（`<Header>`）。
/// - スマホ: タイトルバーは出さず、左上にメニュー（＋戻る）、右上に操作群を浮かせる（`<MobileChrome>`）。
///
/// 右側の並びは **主操作 → アイコン操作 → アシスタント → ⋯** で固定する（画面ごとに順序を
/// 変えさせないため、ReactNode の並びではなくスロットで受ける）。
///
/// ページがアンマウントすると spec は空に戻り、シェルはルート由来の既定表示に戻る。

import * as React from "react";
import Link from "next/link";
import { ChevronLeft, MoreHorizontal, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/// ⋯ メニューの 1 項目。`separatorBefore` で直前に区切りを入れる（破壊的操作の分離など）。
export type ChromeMenuItem = {
  key: string;
  label: string;
  icon?: React.ComponentType<{ className?: string }>;
  onSelect?: () => void;
  href?: string;
  destructive?: boolean;
  disabled?: boolean;
  separatorBefore?: boolean;
  testId?: string;
};

export type PageChromeSpec = {
  /// PC のバーに出すタイトル（スマホでは出さない。本文の先頭に置くのはページの責務）。
  title?: React.ReactNode;
  /// タイトルの補足（保存状態・閲覧のみ など）。PC ではタイトルの右に小さく出す。
  subtitle?: React.ReactNode;
  /// 戻る先。スマホでは左上のメニューの隣、PC ではタイトルの左に出す。
  back?: { href?: string; onClick?: () => void; label?: string };
  /// 主操作（文字ボタン 1 つ・保存／実行など）。`ChromePrimaryButton` を使う。
  primary?: React.ReactNode;
  /// アイコン操作（`ChromeIconButton` を並べる）。主操作とアシスタントの間に出る。
  actions?: React.ReactNode;
  /// AI アシスタントの開閉。常に ⋯ の直前に出す。
  assistant?: { active: boolean; onToggle: () => void; label?: string; testId?: string };
  /// ⋯ メニュー。空なら ⋯ 自体を出さない。
  menu?: ChromeMenuItem[];
  /// ⋯ メニューの先頭に出す見出し（スマホでタイトルを確認する場所・会話名など）。
  menuTitle?: React.ReactNode;
  /// PC のバーだけに出す補助表示（共同編集者のアバター等）。スマホの浮いたボタンには載せない。
  desktopExtra?: React.ReactNode;
  /// スマホで本文を浮いたボタンの下まで流すか（会話など）。false なら本文の上端を空ける。
  flowUnder?: boolean;
};

type SetChrome = (spec: PageChromeSpec | null) => void;

const PageChromeContext = React.createContext<SetChrome | null>(null);
const PageChromeValueContext = React.createContext<PageChromeSpec | null>(null);

export function PageHeaderProvider({ children }: { children: React.ReactNode }) {
  const [spec, setSpec] = React.useState<PageChromeSpec | null>(null);
  return (
    <PageChromeContext.Provider value={setSpec}>
      <PageChromeValueContext.Provider value={spec}>{children}</PageChromeValueContext.Provider>
    </PageChromeContext.Provider>
  );
}

/// シェルが読む: 現在ページが登録したクローム（無ければ null）。
export function usePageChromeValue(): PageChromeSpec | null {
  return React.useContext(PageChromeValueContext);
}

/// ページ側: クロームを登録する。`build` は依存が変わるたびに再評価され、アンマウントで消える。
export function usePageChrome(build: () => PageChromeSpec, deps: React.DependencyList): void {
  const setChrome = React.useContext(PageChromeContext);
  React.useEffect(() => {
    if (!setChrome) return;
    setChrome(build());
    return () => setChrome(null);
    // build は factory。依存は呼び出し側が明示する（title/handlers の元）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setChrome, ...deps]);
}

/// 浮いたボタン群・PC バー共通のアイコンボタン。スマホのカプセル内では 44px になる。
export function ChromeIconButton({
  label,
  onClick,
  pressed,
  className,
  children,
  ...props
}: Omit<React.ComponentProps<"button">, "aria-label"> & { label: string; pressed?: boolean }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={pressed}
      title={label}
      className={cn(
        "flex size-9 shrink-0 items-center justify-center rounded-[10px] text-foreground/70 outline-none",
        "transition-colors duration-[var(--duration-fast)] hover:bg-accent hover:text-foreground",
        "focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94]",
        "max-md:size-11 max-md:hover:bg-sidebar-accent [&_svg]:size-[19px]",
        pressed && "bg-accent text-foreground max-md:bg-sidebar-accent",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

/// 主操作（文字ボタン）。濃色の丸いボタンで、スマホのカプセル内でも PC のバーでも同じ形。
export function ChromePrimaryButton({
  className,
  children,
  ...props
}: React.ComponentProps<"button">) {
  return (
    <button
      type="button"
      className={cn(
        "inline-flex h-9 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[10px] bg-primary px-3.5 text-sm font-semibold text-primary-foreground shadow-[var(--elevation-xs)] outline-none",
        "transition-[background-color,transform] duration-[var(--duration-fast)] hover:bg-primary/90 active:scale-[0.97]",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

/// ⋯ メニュー（PC・スマホ共通。タッチでは項目が 44px になる）。
export function ChromeMenu({
  items,
  title,
}: {
  items: ChromeMenuItem[];
  title?: React.ReactNode;
}) {
  if (items.length === 0) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <ChromeIconButton label="その他の操作" data-testid="chrome-menu">
          <MoreHorizontal aria-hidden />
        </ChromeIconButton>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" collisionPadding={12} className="w-64">
        {title ? (
          <>
            {/* 2 行で省略する。余白（py）と同じ要素に line-clamp を付けると、余白の分だけ 3 行目が
                覗くため、省略は内側の要素で行う。 */}
            <DropdownMenuLabel className="text-[13px] font-semibold text-foreground">
              <span className="line-clamp-2 [overflow-wrap:anywhere]">{title}</span>
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {items.map((item) => (
          <React.Fragment key={item.key}>
            {item.separatorBefore ? <DropdownMenuSeparator /> : null}
            <ChromeMenuEntry item={item} />
          </React.Fragment>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ChromeMenuEntry({ item }: { item: ChromeMenuItem }) {
  const Icon = item.icon;
  const content = (
    <>
      {Icon ? <Icon className="size-4 text-muted-foreground" /> : null}
      {item.label}
    </>
  );
  const className = cn(
    "gap-2.5 px-2.5 py-2",
    item.destructive && "text-destructive focus:text-destructive [&_svg]:text-destructive",
  );
  if (item.href) {
    return (
      <DropdownMenuItem asChild disabled={item.disabled} className={className} data-testid={item.testId}>
        <Link href={item.href}>{content}</Link>
      </DropdownMenuItem>
    );
  }
  return (
    <DropdownMenuItem
      disabled={item.disabled}
      onSelect={item.onSelect}
      className={className}
      data-testid={item.testId}
    >
      {content}
    </DropdownMenuItem>
  );
}

function AssistantToggle({ assistant }: { assistant: NonNullable<PageChromeSpec["assistant"]> }) {
  const label = assistant.label ?? (assistant.active ? "アシスタントを閉じる" : "AI に依頼");
  return (
    <ChromeIconButton
      label={label}
      pressed={assistant.active}
      onClick={assistant.onToggle}
      data-testid={assistant.testId}
    >
      <Sparkles aria-hidden />
    </ChromeIconButton>
  );
}

/// 浮いた操作群の仕切り（Shiki の荒い破線を縦に）。
export function ChromeDivider() {
  return <span aria-hidden className="shiki-dash-y mx-0.5 h-5 w-1.5 shrink-0" />;
}

/// 右側の操作群（主操作 → アイコン → アシスタント → ⋯ の固定順）。PC とスマホで共用する。
/// `divided` のときはグループの間に荒い破線の仕切りを入れる（スマホの浮いた操作群）。
export function ChromeActions({ spec, divided = false }: { spec: PageChromeSpec; divided?: boolean }) {
  const groups = [
    spec.primary,
    spec.actions,
    spec.assistant ? <AssistantToggle assistant={spec.assistant} /> : null,
    spec.menu && spec.menu.length > 0 ? <ChromeMenu items={spec.menu} title={spec.menuTitle} /> : null,
  ].filter(Boolean);
  return (
    <>
      {groups.map((group, i) => (
        <React.Fragment key={i}>
          {divided && i > 0 ? <ChromeDivider /> : null}
          {group}
        </React.Fragment>
      ))}
    </>
  );
}

/// 戻るボタン（href なら Link、onClick ならボタン）。
export function ChromeBackButton({
  back,
  className,
}: {
  back: NonNullable<PageChromeSpec["back"]>;
  className?: string;
}) {
  const label = back.label ?? "戻る";
  if (back.href) {
    return (
      <Link
        href={back.href}
        aria-label={label}
        title={label}
        className={cn(
          "flex size-9 shrink-0 items-center justify-center rounded-[10px] text-foreground/70 outline-none",
          "transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
          "max-md:size-11 max-md:hover:bg-sidebar-accent",
          className,
        )}
      >
        <ChevronLeft className="size-[21px]" aria-hidden />
      </Link>
    );
  }
  return (
    <ChromeIconButton
      label={label}
      onClick={back.onClick}
      className={cn("[&_svg]:size-[21px]", className)}
    >
      <ChevronLeft aria-hidden />
    </ChromeIconButton>
  );
}

/// PC のバーの中身（左に戻る＋タイトル＋補足、右に操作群）。
export function ChromeBar({ spec }: { spec: PageChromeSpec }) {
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2">
      {spec.back ? <ChromeBackButton back={spec.back} /> : null}
      <div className="flex min-w-0 flex-1 items-baseline gap-2.5">
        {spec.title ? (
          <span className="min-w-0 truncate text-sm font-medium text-foreground">{spec.title}</span>
        ) : null}
        {spec.subtitle ? (
          <span className="shrink-0 truncate text-xs text-muted-foreground">{spec.subtitle}</span>
        ) : null}
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {spec.desktopExtra}
        <ChromeActions spec={spec} />
      </div>
    </div>
  );
}

/// スマホでページ本文の先頭に置く見出し（タイトルバーの代わり）。PC では出さない。
export function MobilePageTitle({
  title,
  eyebrow,
  className,
  children,
}: {
  title: React.ReactNode;
  /// 見出しの上の小さな行（保存状態・パンくず など）。
  eyebrow?: React.ReactNode;
  className?: string;
  /// 見出しの右に置く要素（並べ替え等）。
  children?: React.ReactNode;
}) {
  return (
    <div className={cn("flex items-end gap-2 px-4 pb-2 pt-1 md:hidden", className)}>
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        {eyebrow ? <div className="truncate text-[12.5px] text-muted-foreground">{eyebrow}</div> : null}
        <h1 className="text-[26px] font-bold leading-tight tracking-tight">{title}</h1>
      </div>
      {children}
    </div>
  );
}
