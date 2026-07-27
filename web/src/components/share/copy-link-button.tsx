"use client";

import * as React from "react";
import { Check, Copy } from "lucide-react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { cn } from "@/lib/utils";

/// 共有ダイアログ共通の「リンクをコピー」ボタン（#338）。
///
/// リソースのディープリンク URL をクリップボードへコピーするだけのポインタ操作
/// （既存の chat「リンクをコピー」と同じ挙動・トースト文言）。押下時の認可は通常の ReBAC で、
/// 一般アクセスか明示共有の対象だけが開ける。`url` は遅延評価（クリック時に origin を解決）できる。
export function CopyLinkButton({
  url,
  label = "リンクをコピー",
  className,
  size = "sm",
  variant = "outline",
}: {
  url: string | (() => string);
  label?: string;
  className?: string;
  size?: React.ComponentProps<typeof Button>["size"];
  variant?: React.ComponentProps<typeof Button>["variant"];
}) {
  const [copied, setCopied] = React.useState(false);
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null);

  React.useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const copy = async () => {
    const value = typeof url === "function" ? url() : url;
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 2000);
      toast({ description: "リンクをコピーしました。" });
    } catch {
      toast({ variant: "destructive", description: "リンクをコピーできませんでした。" });
    }
  };

  return (
    <Button
      type="button"
      variant={variant}
      size={size}
      className={cn("gap-1.5", className)}
      onClick={() => void copy()}
      data-testid="copy-link"
    >
      {copied ? (
        <Check className="size-4" aria-hidden />
      ) : (
        <Copy className="size-4" aria-hidden />
      )}
      {copied ? "コピーしました" : label}
    </Button>
  );
}
