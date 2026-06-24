"use client";

import * as React from "react";
import { ArrowUp, Mic, Plus, Sparkles } from "lucide-react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/// チャット入力（prompt-kit の PromptInput 相当）。ホームの中央コンポーザと
/// 会話画面の下部入力で共用する。Enter 送信 / Shift+Enter 改行、内容に応じた
/// 自動リサイズ、空のとき送信不可。添付・音声は #70 以降のため tooltip 付き無効。
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
  const textareaRef = React.useRef<HTMLTextAreaElement | null>(null);
  const canSend = value.trim().length > 0 && !disabled;

  // 内容に合わせて高さを自動調整（最大 200px、超えたらスクロール）。
  const resize = React.useCallback(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, []);

  React.useEffect(() => {
    resize();
  }, [value, resize]);

  const submit = () => {
    const text = value.trim();
    if (!text || disabled) return;
    onSubmit(text);
    setValue("");
    // 送信後に高さをリセット。
    requestAnimationFrame(resize);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // IME 変換確定中の Enter は送信しない。
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
      className={cn(
        "rounded-[26px] border border-border bg-card shadow-sm",
        "transition-shadow focus-within:shadow-md focus-within:border-ring/40",
        className,
      )}
    >
      <label htmlFor="composer-input" className="sr-only">
        メッセージを入力
      </label>
      <textarea
        id="composer-input"
        ref={textareaRef}
        rows={1}
        autoFocus={autoFocus}
        disabled={disabled}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        className="max-h-[200px] w-full resize-none bg-transparent px-5 pt-4 text-[15px] leading-relaxed outline-none placeholder:text-muted-foreground/70 disabled:cursor-not-allowed"
      />

      <div className="flex items-center gap-1.5 px-3 pb-3 pt-1">
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              disabled
              aria-label="ファイルを添付（近日対応）"
              className="flex size-9 items-center justify-center rounded-full border border-border text-muted-foreground transition-colors disabled:opacity-60"
            >
              <Plus className="size-[18px]" aria-hidden />
            </button>
          </TooltipTrigger>
          <TooltipContent>添付は近日対応</TooltipContent>
        </Tooltip>

        <span className="flex h-9 items-center gap-1.5 rounded-full border border-border px-3 text-[13px] font-medium text-foreground/80">
          <Sparkles className="size-4 text-foreground/55" aria-hidden />
          標準
        </span>

        <div className="ml-auto flex items-center gap-1.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                disabled
                aria-label="音声入力（近日対応）"
                className="flex size-9 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent disabled:opacity-60 disabled:hover:bg-transparent"
              >
                <Mic className="size-[18px]" aria-hidden />
              </button>
            </TooltipTrigger>
            <TooltipContent>音声入力は近日対応</TooltipContent>
          </Tooltip>

          <button
            type="submit"
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
      </div>
    </form>
  );
}
