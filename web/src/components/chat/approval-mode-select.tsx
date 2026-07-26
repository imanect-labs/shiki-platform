"use client";

import * as React from "react";
import { Check, ChevronDown, ShieldAlert, ShieldCheck, Zap } from "lucide-react";

import { cn } from "@/lib/utils";
import type { AutonomousMode } from "@/lib/chat-api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/// 承認モードの表示定義（#350）。ラベル・説明・アイコンを一箇所に集約する。
const MODES: {
  mode: AutonomousMode;
  label: string;
  description: string;
  icon: React.ComponentType<{ className?: string }>;
  danger?: boolean;
}[] = [
  {
    mode: "require_approval",
    label: "承認必須",
    description: "書込・削除・シェルなど破壊的な操作は毎回承認してから実行します（既定）",
    icon: ShieldCheck,
  },
  {
    mode: "auto",
    label: "オート",
    description: "版管理で戻せる編集は自動承認。削除・シェルなど不可逆な操作だけ承認します",
    icon: Zap,
  },
  {
    mode: "bypass",
    label: "全自動",
    description: "すべての操作を承認なしで実行します（危険・組織ポリシで禁止できます）",
    icon: ShieldAlert,
    danger: true,
  },
];

function modeDef(mode: AutonomousMode) {
  return MODES.find((m) => m.mode === mode) ?? MODES[0];
}

/// 自律スレッドの承認モードセレクタ（実行中トグル可・#350）。
///
/// Claude Code の権限モード切替（shift+tab）に相当する UI。生成中でも切り替えられ、
/// 承認待ちのカードはモード緩和（本人設定のみ有効）で自動的に解決される。
export function ApprovalModeSelect({
  mode,
  onChange,
  bypassAllowed = true,
  className,
}: {
  mode: AutonomousMode;
  onChange: (mode: AutonomousMode) => void;
  /// org 管理者ポリシで bypass（全自動）を選べるか（false なら選択肢を無効化）。
  bypassAllowed?: boolean;
  className?: string;
}) {
  const current = modeDef(mode);
  const Icon = current.icon;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="承認モードを選ぶ"
          title={`承認モード: ${current.label} — ${current.description}`}
          data-testid="approval-mode-trigger"
          className={cn(
            "inline-flex h-9 items-center gap-1.5 rounded-full border px-3 text-[13px] font-medium transition-colors",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
            current.danger
              ? "border-destructive/40 bg-destructive/10 text-destructive"
              : "border-border text-foreground/70 hover:bg-secondary hover:text-foreground",
            className,
          )}
        >
          <Icon className="size-[15px] shrink-0" aria-hidden />
          {current.label}
          <ChevronDown className="size-3.5 opacity-60" aria-hidden />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" sideOffset={8} className="w-80 p-1.5">
        <DropdownMenuLabel className="uppercase tracking-wide">承認モード</DropdownMenuLabel>
        {MODES.map((m) => {
          const ItemIcon = m.icon;
          const disabled = m.mode === "bypass" && !bypassAllowed;
          return (
            <DropdownMenuItem
              key={m.mode}
              disabled={disabled}
              onSelect={() => onChange(m.mode)}
              data-testid={`approval-mode-${m.mode}`}
              className="items-start gap-2.5 px-2.5 py-2"
            >
              <ItemIcon
                className={cn(
                  "mt-0.5 size-4 shrink-0",
                  m.danger ? "text-destructive" : "text-muted-foreground",
                )}
                aria-hidden
              />
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className={cn("flex items-center gap-1.5 text-[13px] font-medium", m.danger && "text-destructive")}>
                  {m.label}
                  {m.danger ? (
                    <span className="rounded bg-destructive/10 px-1 py-px text-[10px] font-semibold text-destructive">
                      危険
                    </span>
                  ) : null}
                  {m.mode === "require_approval" ? (
                    <span className="rounded bg-secondary px-1 py-px text-[10px] text-muted-foreground">
                      既定
                    </span>
                  ) : null}
                </span>
                <span className="text-[12px] leading-snug text-muted-foreground">
                  {disabled ? "組織ポリシで禁止されています" : m.description}
                </span>
              </span>
              {m.mode === mode ? (
                <Check className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
              ) : null}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
