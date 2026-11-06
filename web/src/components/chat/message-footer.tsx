"use client";

import * as React from "react";
import { Check, Copy } from "lucide-react";

import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";

/// アシスタントメッセージ下部のアクション（コピー＋呼び出し側の追加ボタン）。
///
/// 「共有」は右上の会話の共有と紛らわしいので置かない（#1048）。回答の書き出しは
/// 呼び出し側が [`MessageExportMenu`] を children に渡す。
///
/// タッチ端末にはホバーが無いので常に出す（Tailwind v4 の hover/group-hover はタッチで発火しない）。
/// マウスの端末では過去の回答だけホバーで出し、最新の回答（`pinned`）は常に出す。
export function MessageFooter({
  text,
  pinned = false,
  children,
}: {
  text: string;
  /// 常に表示する（最新の回答）。
  pinned?: boolean;
  children?: React.ReactNode;
}) {
  const [copied, setCopied] = React.useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast({ description: "コピーに失敗しました" });
    }
  };

  return (
    <div
      className={cn(
        "mt-1.5 flex items-center gap-0.5 transition-opacity pointer-coarse:-ml-1.5 pointer-coarse:gap-1",
        !pinned && "pointer-fine:opacity-0 pointer-fine:focus-within:opacity-100 pointer-fine:group-hover:opacity-100",
        // 書き出しメニューを開いている間は、ポインタが外れても隠さない（メニューは別の層に出る）。
        !pinned && "pointer-fine:has-[[data-state=open]]:opacity-100",
      )}
    >
      <FooterButton label={copied ? "コピー済み" : "コピー"} onClick={copy}>
        {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
      </FooterButton>
      {children}
    </div>
  );
}

export function FooterButton({
  label,
  onClick,
  pressed,
  children,
}: {
  label: string;
  onClick: () => void;
  /// トグルボタンの押下状態（指定時のみ aria-pressed を付ける）。
  pressed?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={pressed}
      title={label}
      className={footerButtonClass(pressed)}
    >
      {children}
    </button>
  );
}

/// フッターのアイコンボタンの見た目（メニューを開くボタンも同じ形にそろえる）。
export function footerButtonClass(pressed?: boolean): string {
  return cn(
    // タッチでは 40px（見た目のアイコンは同じ大きさ）。
    "flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors pointer-coarse:size-10 pointer-coarse:rounded-full",
    "hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
    "disabled:pointer-events-none disabled:opacity-50",
    pressed && "bg-secondary text-foreground",
  );
}
