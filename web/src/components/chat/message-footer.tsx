"use client";

import * as React from "react";
import { Check, Copy, Share2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/use-toast";

/// アシスタントメッセージ下部のアクション（コピー / シェア＋呼び出し側の追加ボタン）。
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
    <div
      className={cn(
        "mt-1.5 flex items-center gap-0.5 transition-opacity pointer-coarse:-ml-1.5 pointer-coarse:gap-1",
        !pinned && "pointer-fine:opacity-0 pointer-fine:focus-within:opacity-100 pointer-fine:group-hover:opacity-100",
      )}
    >
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
        // タッチでは 40px（見た目のアイコンは同じ大きさ）。
        "flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors pointer-coarse:size-10 pointer-coarse:rounded-full",
        "hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        pressed && "bg-secondary text-foreground",
      )}
    >
      {children}
    </button>
  );
}
