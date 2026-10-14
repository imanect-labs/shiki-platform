import * as React from "react";

import { cn } from "@/lib/utils";

/// 管理コンソールの 1 画面の枠（見出し・説明・右上の操作・本文）。
export function AdminPage({
  title,
  description,
  actions,
  children,
  wide = false,
}: {
  title: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  /// 表など横に広い画面（最大幅を広げる）。
  wide?: boolean;
}) {
  return (
    <div
      className={cn(
        "mx-auto flex w-full flex-col gap-5 px-4 py-6 md:px-8 md:py-8",
        wide ? "max-w-7xl" : "max-w-5xl",
      )}
    >
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
          {description ? (
            <p className="mt-1 text-[13px] text-muted-foreground">{description}</p>
          ) : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </div>
      {children}
    </div>
  );
}

/// 枠線付きのパネル（見出し＋本文）。
export function Panel({
  title,
  description,
  actions,
  children,
  className,
}: {
  title?: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("rounded-lg border bg-card", className)}>
      {title ? (
        <div className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">{title}</h2>
            {description ? (
              <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>
            ) : null}
          </div>
          {actions}
        </div>
      ) : null}
      {children}
    </section>
  );
}

/// 指標タイル（ラベル・値・補足）。
export function StatTile({
  label,
  value,
  sub,
  children,
}: {
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="rounded-lg border bg-card px-4 py-3.5">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1.5 text-2xl font-semibold tabular-nums tracking-tight">{value}</div>
      {sub ? <div className="mt-0.5 text-xs text-muted-foreground">{sub}</div> : null}
      {children}
    </div>
  );
}

/// 設定フォームの 1 行（左にラベル・説明、右に入力）。Supabase の設定画面と同じ並び。
export function SettingRow({
  label,
  description,
  htmlFor,
  children,
}: {
  label: string;
  description?: React.ReactNode;
  htmlFor?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="grid gap-3 border-t px-4 py-4 first:border-t-0 md:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)] md:gap-6">
      <div>
        <label htmlFor={htmlFor} className="text-[13px] font-medium">
          {label}
        </label>
        {description ? (
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{description}</p>
        ) : null}
      </div>
      <div className="flex min-w-0 items-start">{children}</div>
    </div>
  );
}
