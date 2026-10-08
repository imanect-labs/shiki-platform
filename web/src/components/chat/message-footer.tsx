"use client";

import * as React from "react";
import { Check, Copy, Share2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";

/// アシスタントメッセージ下部のアクション（コピー / シェア＋呼び出し側の追加ボタン）。
export function MessageFooter({ text, children }: { text: string; children?: React.ReactNode }) {
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

  const share = async () => {
    // Web Share API があれば OS 共有、無ければ Markdown をクリップボードへ。
    const nav = navigator as Navigator & { share?: (d: ShareData) => Promise<void> };
    try {
      if (typeof nav.share === "function") {
        await nav.share({ text });
        return;
      }
      await nav.clipboard.writeText(text);
      toast({ description: "共有用にテキストをコピーしました" });
    } catch (e) {
      // ユーザーがネイティブ共有シートを閉じた場合（AbortError）は無視。
      // それ以外（権限拒否・クリップボード失敗など）は copy() と揃えて通知する。
      if (e instanceof DOMException && e.name === "AbortError") return;
      toast({ description: "共有に失敗しました" });
    }
  };

  return (
    <div className="mt-1.5 flex items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
      <FooterButton label={copied ? "コピー済み" : "コピー"} onClick={copy}>
        {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
      </FooterButton>
      <FooterButton label="共有" onClick={share}>
        <Share2 className="size-3.5" />
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
      className={cn(
        "flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors",
        "hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        pressed && "bg-secondary text-foreground",
      )}
    >
      {children}
    </button>
  );
}
