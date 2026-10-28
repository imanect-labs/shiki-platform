"use client";

// prompt-kit (https://www.prompt-kit.com) の PromptInput を本リポジトリへ vendoring
// したもの（shadcn 同様にソースを取り込んで保守する方式）。MIT License。
// 変更点: import を本リポジトリの primitive に合わせ、日本語 IME 変換確定中の
// Enter を送信扱いしないガード（isComposing / keyCode 229）を追加している。
// タッチ端末（pointer: coarse）では Enter を改行にし（送信はボタンのみ）、autoFocus は
// マウスの端末だけで効かせる（開いた瞬間にソフトキーボードを出さない）。
// 送信キーを Ctrl/⌘+Enter にできる（`submitKey`・ユーザー設定 #952。Enter は改行になる）。
import * as React from "react";
import {
  createContext,
  useContext,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import { Textarea } from "@/components/ui/textarea";
import { useMediaQuery } from "@/hooks/use-media-query";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

type PromptInputContextType = {
  isLoading: boolean;
  value: string;
  setValue: (value: string) => void;
  maxHeight: number | string;
  onSubmit?: () => void;
  disabled?: boolean;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
};

const PromptInputContext = createContext<PromptInputContextType>({
  isLoading: false,
  value: "",
  setValue: () => {},
  maxHeight: 240,
  onSubmit: undefined,
  disabled: false,
  textareaRef: React.createRef<HTMLTextAreaElement>(),
});

function usePromptInput() {
  return useContext(PromptInputContext);
}

export type PromptInputProps = {
  isLoading?: boolean;
  value?: string;
  onValueChange?: (value: string) => void;
  maxHeight?: number | string;
  onSubmit?: () => void;
  children: React.ReactNode;
  className?: string;
  disabled?: boolean;
} & React.ComponentProps<"div">;

function PromptInput({
  className,
  isLoading = false,
  maxHeight = 240,
  value,
  onValueChange,
  onSubmit,
  children,
  disabled = false,
  onClick,
  ...props
}: PromptInputProps) {
  const [internalValue, setInternalValue] = useState(value || "");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const handleChange = (newValue: string) => {
    setInternalValue(newValue);
    onValueChange?.(newValue);
  };

  const handleClick: React.MouseEventHandler<HTMLDivElement> = (e) => {
    // 余白を押したら入力欄へ。ボタン・リンク・メニュー項目を押したときは移さない（タッチでは
    // フォーカスがソフトキーボードを出し、開いたメニューやシートを隠してしまう）。ポータルで
    // 描かれた中身（シート・ポップオーバー）のクリックも React のツリーを伝ってここへ届くので、
    // DOM 上この枠の外なら何もしない。
    const target = e.target as Element | null;
    const inside = target && e.currentTarget.contains(target);
    const onControl = target?.closest(
      "button,a,input,select,[role=menuitem],[role=switch],[role=option],[role=radio]",
    );
    if (!disabled && inside && !onControl) textareaRef.current?.focus();
    onClick?.(e);
  };

  return (
    <TooltipProvider>
      <PromptInputContext.Provider
        value={{
          isLoading,
          value: value ?? internalValue,
          setValue: onValueChange ?? handleChange,
          maxHeight,
          onSubmit,
          disabled,
          textareaRef,
        }}
      >
        <div
          onClick={handleClick}
          className={cn(
            "border-input bg-background cursor-text rounded-3xl border p-2 shadow-xs",
            disabled && "cursor-not-allowed opacity-60",
            className,
          )}
          {...props}
        >
          {children}
        </div>
      </PromptInputContext.Provider>
    </TooltipProvider>
  );
}

export type PromptInputTextareaProps = {
  disableAutosize?: boolean;
  /// 送信キー。`enter`（既定）は Enter で送信・Shift+Enter で改行、`mod_enter` は Ctrl/⌘+Enter で
  /// 送信・Enter で改行。
  submitKey?: "enter" | "mod_enter";
} & React.ComponentProps<typeof Textarea>;

/// タッチが主の端末か（Enter を改行にする・autoFocus しない判定に使う）。
function isCoarsePointer(): boolean {
  return typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;
}

function PromptInputTextarea({
  className,
  onKeyDown,
  disableAutosize = false,
  autoFocus = false,
  enterKeyHint,
  submitKey = "enter",
  ...props
}: PromptInputTextareaProps) {
  const { value, setValue, maxHeight, onSubmit, disabled, textareaRef } =
    usePromptInput();
  // マウスの端末だけ開いた瞬間にフォーカスする。`autoFocus` 属性は初回描画（ハイドレーション
  // 中はメディアクエリが分からない）で決まってしまうので、マウント後に判定して自分で移す。
  React.useEffect(() => {
    if (!autoFocus || typeof window === "undefined") return;
    // 利用者が既に別の入力欄で打っているとき（会話を開いた直後にサイドバーで名前を変える等）は
    // 奪わない（奪うとその欄の blur で入力が確定・破棄される）。
    const active = document.activeElement;
    const typing =
      active instanceof HTMLElement &&
      (active.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(active.tagName));
    if (typing) return;
    if (window.matchMedia("(pointer: fine)").matches) {
      textareaRef.current?.focus({ preventScroll: true });
    }
    // マウント時だけ（スレッドを開いたとき・ホーム）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Enter のキー表示（送信 / 改行）。タッチでは Enter が改行なので「改行」を出す。
  const coarse = useMediaQuery("(pointer: coarse)");

  const adjustHeight = (el: HTMLTextAreaElement | null) => {
    if (!el || disableAutosize) return;

    el.style.height = "auto";
    // 空のときは 1 行に固定する（長いプレースホルダが折り返して高さを押し上げないように）。
    if (!el.value) {
      el.style.height = "";
      return;
    }

    if (typeof maxHeight === "number") {
      el.style.height = `${Math.min(el.scrollHeight, maxHeight)}px`;
    } else {
      el.style.height = `min(${el.scrollHeight}px, ${maxHeight})`;
    }
  };

  const handleRef = (el: HTMLTextAreaElement | null) => {
    textareaRef.current = el;
    adjustHeight(el);
  };

  useLayoutEffect(() => {
    if (!textareaRef.current || disableAutosize) return;

    const el = textareaRef.current;
    el.style.height = "auto";
    if (!el.value) {
      el.style.height = "";
      return;
    }

    if (typeof maxHeight === "number") {
      el.style.height = `${Math.min(el.scrollHeight, maxHeight)}px`;
    } else {
      el.style.height = `min(${el.scrollHeight}px, ${maxHeight})`;
    }
  }, [value, maxHeight, disableAutosize, textareaRef]);

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    adjustHeight(e.target);
    setValue(e.target.value);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // 呼び出し側に先に処理させる（スラッシュコマンド補完が Enter で候補を確定する等・#387）。
    // 先に送信してしまうと補完の Enter が握れない。`preventDefault()` した場合は送信しない。
    onKeyDown?.(e);
    if (e.defaultPrevented) return;
    // IME 変換確定中（日本語入力）の Enter は送信せず確定に使わせる。Safari は確定の Enter で
    // isComposing を落とすことがあるので keyCode 229 も見る。
    const composing = e.nativeEvent.isComposing || e.keyCode === 229;
    if (e.key !== "Enter" || composing) return;
    if (submitKey === "mod_enter") {
      // Ctrl/⌘ を添えたときだけ送る（修飾キーは明示の操作なので、タッチ端末の外付けキーボードでも送る）。
      // 素の Enter・Shift+Enter は既定の改行のまま。
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        onSubmit?.();
      }
      return;
    }
    // タッチ端末の Enter は改行（ソフトキーボードの Enter で誤送信しない。送信はボタンだけ）。
    if (!e.shiftKey && !isCoarsePointer()) {
      e.preventDefault();
      onSubmit?.();
    }
  };

  return (
    <Textarea
      ref={handleRef}
      value={value}
      onChange={handleChange}
      onKeyDown={handleKeyDown}
      className={cn(
        "min-h-[44px] w-full resize-none border-none bg-transparent text-foreground shadow-none outline-none focus-visible:ring-0 focus-visible:ring-offset-0",
        className,
      )}
      rows={1}
      disabled={disabled}
      enterKeyHint={enterKeyHint ?? (coarse || submitKey === "mod_enter" ? "enter" : "send")}
      {...props}
    />
  );
}

export type PromptInputActionsProps = React.HTMLAttributes<HTMLDivElement>;

function PromptInputActions({
  children,
  className,
  ...props
}: PromptInputActionsProps) {
  return (
    <div className={cn("flex items-center gap-2", className)} {...props}>
      {children}
    </div>
  );
}

export type PromptInputActionProps = {
  className?: string;
  tooltip: React.ReactNode;
  children: React.ReactNode;
  side?: "top" | "bottom" | "left" | "right";
} & React.ComponentProps<typeof Tooltip>;

function PromptInputAction({
  tooltip,
  children,
  className,
  side = "top",
  ...props
}: PromptInputActionProps) {
  const { disabled } = usePromptInput();

  return (
    <Tooltip {...props}>
      <TooltipTrigger
        asChild
        disabled={disabled}
        onClick={(event) => event.stopPropagation()}
      >
        {children}
      </TooltipTrigger>
      <TooltipContent side={side} className={className}>
        {tooltip}
      </TooltipContent>
    </Tooltip>
  );
}

export {
  PromptInput,
  PromptInputTextarea,
  PromptInputActions,
  PromptInputAction,
  usePromptInput,
};
