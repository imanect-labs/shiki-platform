"use client";

import * as React from "react";
import { Leaf, type LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { currentSeasonIndex, seasonVar } from "@/lib/season";

/// ユーザー設定の部品。管理コンソール（AdminPage/Panel）と同じ並びを保ちつつ、ホーム画面と同じ
/// シキの言葉づかい（荒い破線の「間」・角の大きい面・触れたときだけ灯る四季の差し色）で描く。

/// スマホの本文先頭に出す現在地（「設定・セキュリティ」）。スマホにはタイトルバーが無いので、
/// SettingsShell がここへ渡し、SettingsPage が見出しの上に出す。
export const SettingsCrumbContext = React.createContext<string | null>(null);

/// 1 画面の枠（見出し・説明・右上の操作・本文）。設定は読み物に近いので本文幅は狭めに取る。
export function SettingsPage({
  title,
  description,
  actions,
  children,
}: {
  title: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
}) {
  const crumb = React.useContext(SettingsCrumbContext);
  return (
    <div className="mx-auto flex w-full max-w-[46rem] flex-col gap-9 px-4 pb-16 pt-1 md:px-8 md:pt-11">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          {crumb ? <div className="truncate text-[12.5px] text-muted-foreground md:hidden">{crumb}</div> : null}
          <h1 className="text-[26px] font-bold leading-tight tracking-[-0.02em]">{title}</h1>
          {description ? (
            <p className="mt-1.5 text-[13.5px] leading-relaxed text-muted-foreground">{description}</p>
          ) : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </div>
      {children}
    </div>
  );
}

/// 見出し付きのまとまり。見出しは面の外に置き、面の中は行だけにする（行の間は控えめな破線）。
export function SettingsSection({
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
    <section className={cn("flex flex-col gap-3", className)}>
      {title ? (
        <div className="flex flex-wrap items-end justify-between gap-x-3 gap-y-2 px-1">
          <div className="min-w-0">
            <h2 className="flex items-center gap-2 text-[15px] font-semibold tracking-[-0.01em]">{title}</h2>
            {description ? (
              <p className="mt-0.5 text-[12.5px] leading-relaxed text-muted-foreground">{description}</p>
            ) : null}
          </div>
          {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
        </div>
      ) : null}
      <div className="rule-soft overflow-hidden rounded-2xl border bg-card shadow-xs">{children}</div>
    </section>
  );
}

/// 面の中の 1 行（左にラベルと説明・右に操作）。`stacked` は入力が横に広い行（テキストエリア等）。
export function SettingsRow({
  label,
  description,
  htmlFor,
  stacked = false,
  children,
}: {
  label: React.ReactNode;
  description?: React.ReactNode;
  htmlFor?: string;
  stacked?: boolean;
  children?: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "shiki-dash-top flex flex-col gap-3 px-5 py-4 first:bg-none",
        !stacked && "sm:flex-row sm:items-center sm:justify-between sm:gap-8",
      )}
    >
      <div className="min-w-0">
        <label htmlFor={htmlFor} className="text-[13.5px] font-medium">
          {label}
        </label>
        {description ? (
          <p className="mt-0.5 text-[12.5px] leading-relaxed text-muted-foreground">{description}</p>
        ) : null}
      </div>
      {children !== undefined ? (
        <div className={cn("flex min-w-0 items-center", !stacked && "sm:shrink-0 sm:justify-end")}>{children}</div>
      ) : null}
    </div>
  );
}

/// 一覧の 1 行（アイコンの台・名前・補足・右端の操作）。
export function SettingsItem({
  icon,
  title,
  meta,
  action,
}: {
  icon: React.ReactNode;
  title: React.ReactNode;
  meta?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <li className="shiki-dash-top flex items-center gap-3.5 px-5 py-3.5 first:bg-none">
      {icon}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-[13.5px] font-medium">{title}</div>
        {meta ? (
          <div className="mt-0.5 flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground">{meta}</div>
        ) : null}
      </div>
      {action}
    </li>
  );
}

/// アイコンの台（ホーム画面のショートカットと同じ角の大きい面）。`season` を渡すとその季節の色に灯す。
export function IconTile({ icon: Icon, season }: { icon: LucideIcon; season?: number }) {
  return (
    <span
      aria-hidden
      style={season != null ? { color: seasonVar(season) } : undefined}
      className={cn(
        "flex size-10 shrink-0 items-center justify-center rounded-xl border bg-background",
        season == null && "text-foreground/70",
      )}
    >
      <Icon className="size-[18px]" />
    </span>
  );
}

/// 状態の小さな印（「有効」「このブラウザ」など）。色の面を敷かず、点ひとつで示す。
export function StatusDot({ on = true, children }: { on?: boolean; children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-xs font-normal text-muted-foreground">
      <span
        aria-hidden
        className={cn("size-1.5 rounded-full", on ? "bg-[var(--season-summer)]" : "bg-muted-foreground/40")}
      />
      {children}
    </span>
  );
}

/// まだ動かない項目の印（ホーム画面の「準備中」と同じ）。
export function SoonBadge({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        "rounded bg-muted px-1.5 py-px text-[10px] font-normal leading-4 text-muted-foreground",
        className,
      )}
    >
      準備中
    </span>
  );
}

/// 準備中の画面の先頭に置く一言（表示はサンプルで、変更は保存されない）。
export function PreviewNote() {
  return (
    <div className="flex items-center gap-2.5 rounded-xl border border-dashed px-4 py-3 text-[13px] text-muted-foreground">
      <Leaf
        className="size-4 shrink-0"
        style={{ color: seasonVar(currentSeasonIndex()) }}
        strokeWidth={2.5}
        aria-hidden
        suppressHydrationWarning
      />
      準備中の画面です。表示はサンプルで、変更は保存されません。
    </div>
  );
}

/// 空のときの一言（面の中に置く）。
export function SettingsEmpty({ children }: { children: React.ReactNode }) {
  return <p className="px-5 py-7 text-center text-[13px] text-muted-foreground">{children}</p>;
}
