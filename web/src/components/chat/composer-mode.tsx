"use client";

/// スマホのコンポーザの「実行モード」ボタンとカード。
///
/// PC ではエージェントモードのトグル・ワークスペース・承認モード（承認必須/オート/全自動）を
/// 入力欄の下に並べるが、1 行の入力バーでは入らない（「エージェントモ／ード」がボタン内で折れる）。
/// スマホでは 1 つのアイコンボタンにまとめ、押すと説明付きのカード（下からのシート）で選ぶ。
/// ON の状態は塗り＋枠と、入力欄のプレースホルダ（「エージェント・承認必須」等）で伝える。

import * as React from "react";
import { Bot, Check, ChevronRight, HardDrive, MessageCircle } from "lucide-react";

import { cn } from "@/lib/utils";
import type { AutonomousMode, WorkspaceChoice } from "@/lib/chat-api";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { APPROVAL_MODES, approvalModeDef } from "./approval-mode-select";

export function ComposerModeButton({
  autonomous,
  onAutonomousChange,
  approvalMode,
  onApprovalModeChange,
  bypassAllowed,
  workspace,
  onPickWorkspace,
  className,
}: {
  autonomous: boolean;
  onAutonomousChange: (v: boolean) => void;
  approvalMode: AutonomousMode | null;
  onApprovalModeChange?: (m: AutonomousMode) => void;
  bypassAllowed: boolean;
  /// ワークスペースの作成場所（ホーム）。未指定なら行を出さない。
  workspace?: WorkspaceChoice | null;
  onPickWorkspace?: () => void;
  className?: string;
}) {
  const [open, setOpen] = React.useState(false);
  // ワークスペースの選択はカードが閉じきってから開く（閉じる側の後始末で即座に閉じないように）。
  const afterClose = React.useRef<(() => void) | null>(null);
  const approval = autonomous && approvalMode ? approvalModeDef(approvalMode) : null;
  const danger = !!approval?.danger;
  const label = autonomous
    ? `実行モード: エージェント${approval ? `（${approval.label}）` : ""}`
    : "実行モード: チャット";

  return (
    <>
      <button
        type="button"
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        data-testid="composer-mode-button"
        onClick={() => setOpen(true)}
        className={cn(
          "flex size-10 shrink-0 items-center justify-center rounded-full border transition-colors",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          autonomous
            ? danger
              ? "border-destructive/40 bg-destructive/10 text-destructive"
              : "border-primary/30 bg-primary/10 text-primary"
            : "border-transparent text-foreground/70 hover:bg-secondary hover:text-foreground",
          className,
        )}
      >
        <Bot className="size-5" aria-hidden />
      </button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          className="gap-5 sm:max-w-md"
          data-testid="composer-mode-card"
          onCloseAutoFocus={(e) => {
            const run = afterClose.current;
            if (!run) return;
            afterClose.current = null;
            e.preventDefault();
            run();
          }}
        >
          <div className="pr-10">
            <DialogTitle className="text-base">実行モード</DialogTitle>
            <DialogDescription className="mt-1">
              ふだんはチャットで十分です。調べて資料を作るなど、手順の多い依頼はエージェントへ。
            </DialogDescription>
          </div>

          <OptionGroup label="モード">
            <OptionCard
              icon={MessageCircle}
              title="チャット"
              description="質問に答えます。必要なときは検索などのツールも自動で使います。"
              selected={!autonomous}
              onSelect={() => onAutonomousChange(false)}
              testId="composer-mode-chat"
            />
            <OptionCard
              icon={Bot}
              title="エージェント"
              description="計画を立て、ワークスペースで作業を進めます。危ない操作は承認を求めます。"
              selected={autonomous}
              onSelect={() => onAutonomousChange(true)}
              testId="composer-mode-agent"
            />
          </OptionGroup>

          {autonomous && onPickWorkspace ? (
            <button
              type="button"
              onClick={() => {
                afterClose.current = onPickWorkspace;
                setOpen(false);
              }}
              className="-mt-2 flex h-12 w-full items-center gap-3 rounded-xl border border-border px-3 text-left text-[15px] transition-colors active:bg-accent"
            >
              <HardDrive className="size-[18px] shrink-0 text-muted-foreground" aria-hidden />
              <span className="flex-1">ワークスペース</span>
              <span className="max-w-[45%] truncate text-sm text-muted-foreground">
                {workspace ? workspace.folderName : "マイドライブ"}
              </span>
              <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            </button>
          ) : null}

          {autonomous && approvalMode && onApprovalModeChange ? (
            <OptionGroup label="承認">
              {APPROVAL_MODES.map((m) => {
                const disabled = m.mode === "bypass" && !bypassAllowed;
                return (
                  <OptionCard
                    key={m.mode}
                    icon={m.icon}
                    title={m.label}
                    badge={m.danger ? "危険" : m.mode === "require_approval" ? "既定" : undefined}
                    danger={m.danger}
                    description={disabled ? "組織ポリシで禁止されています" : m.description}
                    selected={m.mode === approvalMode}
                    disabled={disabled}
                    onSelect={() => onApprovalModeChange(m.mode)}
                    testId={`composer-approval-${m.mode}`}
                  />
                );
              })}
            </OptionGroup>
          ) : null}

          <button
            type="button"
            onClick={() => setOpen(false)}
            className="h-11 w-full rounded-full bg-primary text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
          >
            完了
          </button>
        </DialogContent>
      </Dialog>
    </>
  );
}

function OptionGroup({ label, children }: { label: string; children: React.ReactNode }) {
  const id = React.useId();
  return (
    <div role="radiogroup" aria-labelledby={id} className="flex flex-col gap-2">
      <span id={id} className="px-1 text-[12px] font-semibold tracking-wide text-muted-foreground">
        {label}
      </span>
      {children}
    </div>
  );
}

function OptionCard({
  icon: Icon,
  title,
  description,
  badge,
  danger,
  selected,
  disabled,
  onSelect,
  testId,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description: string;
  badge?: string;
  danger?: boolean;
  selected: boolean;
  disabled?: boolean;
  onSelect: () => void;
  testId?: string;
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      data-testid={testId}
      className={cn(
        "flex w-full items-start gap-3 rounded-xl border px-3 py-3 text-left transition-colors",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        "disabled:cursor-not-allowed disabled:opacity-50",
        selected ? "border-transparent bg-accent" : "border-border active:bg-accent/60",
      )}
    >
      <Icon
        className={cn("mt-0.5 size-[18px] shrink-0", danger ? "text-destructive" : "text-muted-foreground")}
        aria-hidden
      />
      <span className="min-w-0 flex-1">
        <span className={cn("flex items-center gap-1.5 text-[15px] font-medium", danger && "text-destructive")}>
          {title}
          {badge ? (
            <span
              className={cn(
                "rounded px-1 py-px text-[10.5px] font-semibold",
                danger ? "bg-destructive/10 text-destructive" : "bg-secondary text-muted-foreground",
              )}
            >
              {badge}
            </span>
          ) : null}
        </span>
        <span className="mt-0.5 block text-[12.5px] leading-snug text-muted-foreground">{description}</span>
      </span>
      <span className="flex size-5 shrink-0 items-center justify-center">
        {selected ? <Check className="size-[18px] text-primary" aria-hidden /> : null}
      </span>
    </button>
  );
}
