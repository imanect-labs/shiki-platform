"use client";

import * as React from "react";
import Link from "next/link";
import { ChevronsUpDown, LogIn, LogOut, Settings, ShieldCheck } from "lucide-react";

import { cn, initialsFrom } from "@/lib/utils";
import { login, logout } from "@/lib/auth";
import { useMe } from "@/hooks/use-me";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ThemeToggle } from "@/components/ui/theme-toggle";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/// 最下部のアカウントバナー。/me（表示名＋メール）を表示し、クリックで設定・テーマ・ログアウトの
/// メニューを開く。未認証ならログイン導線、ロード中はスケルトン。
/// スマホのドロワ（`mobile`）は行を大きくし、右に設定への直通ボタン（44px）を添える。
export function SidebarAccount({
  collapsed,
  mobile = false,
  onNavigate,
}: {
  collapsed: boolean;
  mobile?: boolean;
  onNavigate?: () => void;
}) {
  const { data, unauthenticated, loading } = useMe();

  if (loading) {
    return (
      <div
        className={cn(
          "flex items-center gap-3 p-3",
          collapsed && "justify-center",
          mobile && "px-4 pb-[max(0.75rem,env(safe-area-inset-bottom))]",
        )}
      >
        <Skeleton className="size-8 rounded-full" />
        {!collapsed ? (
          <div className="flex flex-1 flex-col gap-1.5">
            <Skeleton className="h-3 w-24" />
            <Skeleton className="h-2.5 w-32" />
          </div>
        ) : null}
      </div>
    );
  }

  if (unauthenticated || !data) {
    return (
      <div className={cn("p-2", mobile && "px-3 pb-[max(0.5rem,env(safe-area-inset-bottom))]")}>
        <Button
          variant="default"
          size={collapsed ? "icon" : "default"}
          className={cn(!collapsed && "w-full")}
          onClick={() => login()}
          aria-label="ログイン"
        >
          <LogIn className="size-4" aria-hidden />
          {!collapsed ? "ログイン" : null}
        </Button>
      </div>
    );
  }

  // 表示名はメールのローカル部（無ければ id）から導出する（表示名フィールドは後続の
  // BFF/バックエンド PR で対応）。2 行目はメール（無ければ組織）で、同じ文字列を 2 回出さない。
  const displayName = data.email ? data.email.slice(0, data.email.indexOf("@")) || data.email : data.id;
  const subLine = data.email ?? data.org;
  const initials = initialsFrom(data.email ?? data.id);

  const trigger = (
    <button
      type="button"
      aria-label="アカウントメニューを開く"
      className={cn(
        "flex items-center gap-3 rounded-lg outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        collapsed
          ? "justify-center p-1"
          : "min-w-0 flex-1 p-2 hover:bg-sidebar-accent active:bg-sidebar-accent max-md:min-h-14",
      )}
    >
      <Avatar>
        <AvatarFallback>{initials}</AvatarFallback>
      </Avatar>
      {!collapsed ? (
        <>
          <span className="flex min-w-0 flex-1 flex-col text-left">
            <span className="truncate text-sm font-medium text-sidebar-foreground max-md:text-[15px]">
              {displayName}
            </span>
            <span className="truncate text-xs text-sidebar-foreground/50 max-md:text-[13px]">
              {subLine}
            </span>
          </span>
          <ChevronsUpDown className="size-4 shrink-0 text-sidebar-foreground/50" aria-hidden />
        </>
      ) : null}
    </button>
  );

  return (
    <div
      className={cn(
        "flex items-center gap-1 p-2",
        collapsed && "justify-center",
        mobile && "px-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]",
      )}
    >
      <DropdownMenu>
        {collapsed ? (
          // 折りたたみ時は Tooltip と Dropdown の両トリガーを同じボタンへ重ねる。
          // （DropdownMenuTrigger asChild の子を Tooltip 根にすると trigger が
          //   ボタンに配線されず、メニューがレールから開けなくなるため）
          <Tooltip>
            <TooltipTrigger asChild>
              <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
            </TooltipTrigger>
            <TooltipContent side="right">{displayName}</TooltipContent>
          </Tooltip>
        ) : (
          <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
        )}
        <DropdownMenuContent
          align="start"
          side="top"
          sideOffset={8}
          className="w-[var(--radix-dropdown-menu-trigger-width)] min-w-56"
        >
          <DropdownMenuLabel className="flex flex-col gap-0.5 normal-case">
            <span className="truncate text-sm font-medium text-foreground">{displayName}</span>
            {data.email ? (
              <span className="truncate text-xs text-muted-foreground">{data.email}</span>
            ) : null}
            <span className="truncate text-[11px] text-muted-foreground/70">
              {data.org} · {data.tenant_id}
            </span>
          </DropdownMenuLabel>
          <DropdownMenuSeparator />
          <div className="flex items-center justify-between gap-2 px-2 py-1.5">
            <span className="text-sm">テーマ</span>
            <ThemeToggle className="pointer-coarse:[&_button]:size-10" />
          </div>
          <DropdownMenuSeparator />
          <DropdownMenuItem asChild>
            <Link href="/settings" onClick={onNavigate}>
              <Settings aria-hidden />
              設定
            </Link>
          </DropdownMenuItem>
          {data.is_admin ? (
            <DropdownMenuItem asChild>
              <Link href="/admin" onClick={onNavigate}>
                <ShieldCheck aria-hidden />
                管理コンソール
              </Link>
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem variant="destructive" onSelect={() => void logout()}>
            <LogOut aria-hidden />
            ログアウト
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {mobile ? (
        <Link
          href="/settings"
          onClick={onNavigate}
          aria-label="設定"
          className="flex size-11 shrink-0 items-center justify-center rounded-lg text-sidebar-foreground/60 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring active:bg-sidebar-accent"
        >
          <Settings className="size-5" aria-hidden />
        </Link>
      ) : null}
    </div>
  );
}
