"use client";

import * as React from "react";
import { ArrowUp, Mic, Plus, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  PromptInput,
  PromptInputActions,
  PromptInputAction,
  PromptInputTextarea,
} from "@/components/prompt-kit/prompt-input";

/// チャット入力。prompt-kit の PromptInput をベースに、ホームの中央コンポーザと
/// 会話画面の下部入力で共用する。Enter 送信 / Shift+Enter 改行（IME 変換中は送信
/// しない）、内容に応じた自動リサイズ、空のとき送信不可。添付・音声は今後対応のため
/// tooltip 付きで無効表示にする。
export function Composer({
  onSubmit,
  placeholder = "何でも尋ねて、何でも作成",
  autoFocus = false,
  disabled = false,
  className,
}: {
  onSubmit: (text: string) => void;
  placeholder?: string;
  autoFocus?: boolean;
  disabled?: boolean;
  className?: string;
}) {
  const [value, setValue] = React.useState("");
  const canSend = value.trim().length > 0 && !disabled;

  const submit = () => {
    const text = value.trim();
    if (!text || disabled) return;
    onSubmit(text);
    setValue("");
  };

  return (
    <PromptInput
      value={value}
      onValueChange={setValue}
      onSubmit={submit}
      disabled={disabled}
      maxHeight={200}
      className={cn(
        // フォーカス時は濃い枠ではなく淡いリングで示す（黒枠に見えないように）。
        "rounded-[26px] border-border bg-card shadow-sm transition-shadow",
        "focus-within:border-ring/25 focus-within:shadow-md focus-within:ring-4 focus-within:ring-ring/10",
        className,
      )}
    >
      <PromptInputTextarea
        placeholder={placeholder}
        autoFocus={autoFocus}
        aria-label="メッセージを入力"
        className="px-3 pt-2 pb-1 text-[15px] leading-relaxed placeholder:text-muted-foreground/70"
      />

      <PromptInputActions className="justify-between px-1 pb-1">
        <div className="flex items-center gap-1.5">
          <PromptInputAction tooltip="添付は近日対応">
            <button
              type="button"
              aria-disabled="true"
              aria-label="ファイルを添付（近日対応）"
              onClick={(e) => e.preventDefault()}
              className="flex size-9 cursor-not-allowed items-center justify-center rounded-full border border-border text-muted-foreground opacity-60"
            >
              <Plus className="size-[18px]" aria-hidden />
            </button>
          </PromptInputAction>

          <span className="flex h-9 items-center gap-1.5 rounded-full border border-border px-3 text-[13px] font-medium text-foreground/80">
            <Sparkles className="size-4 text-foreground/55" aria-hidden />
            標準
          </span>
        </div>

        <div className="flex items-center gap-1.5">
          <PromptInputAction tooltip="音声入力は近日対応">
            <button
              type="button"
              aria-disabled="true"
              aria-label="音声入力（近日対応）"
              onClick={(e) => e.preventDefault()}
              className="flex size-9 cursor-not-allowed items-center justify-center rounded-full text-muted-foreground opacity-60"
            >
              <Mic className="size-[18px]" aria-hidden />
            </button>
          </PromptInputAction>

          <button
            type="button"
            onClick={submit}
            disabled={!canSend}
            aria-label="送信"
            className={cn(
              "flex size-9 items-center justify-center rounded-full transition-colors",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
              canSend
                ? "bg-primary text-primary-foreground hover:bg-primary/90"
                : "bg-muted text-muted-foreground",
            )}
          >
            <ArrowUp className="size-[18px]" aria-hidden />
          </button>
        </div>
      </PromptInputActions>
    </PromptInput>
  );
}
