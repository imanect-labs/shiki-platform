"use client";

/// 会話画面（/c/[id]）のヘッダアクション。統一ヘッダスロットへ注入して使う。
/// 共有ボタン＋⋯メニュー（リンクをコピー / 設定へ）。スレッドのリネーム/削除は
/// backend 未提供（/threads/{id} は GET のみ）のため、実在する操作のみ置く。

import { Copy, MoreHorizontal, Settings, Share2 } from "lucide-react";
import Link from "next/link";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import { PageHeaderBar, usePageHeader } from "@/components/shell/page-header-context";

export function ChatHeaderActions({ onShare }: { onShare: () => void }) {
  const copyLink = async () => {
    try {
      await navigator.clipboard.writeText(window.location.href);
      toast({ description: "リンクをコピーしました。" });
    } catch {
      toast({ description: "リンクをコピーできませんでした。" });
    }
  };

  return (
    <>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onShare}
        className="gap-1.5 text-foreground/70 hover:text-foreground"
      >
        <Share2 className="size-4" aria-hidden />
        共有
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="size-8 text-foreground/60 hover:text-foreground"
            aria-label="このチャットの設定"
          >
            <MoreHorizontal className="size-[18px]" aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-48">
          <DropdownMenuItem onSelect={() => void copyLink()}>
            <Copy className="size-4" aria-hidden />
            リンクをコピー
          </DropdownMenuItem>
          <DropdownMenuItem asChild>
            <Link href="/settings">
              <Settings className="size-4" aria-hidden />
              設定
            </Link>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
}

/// 会話画面（page variant）専用: 統一ヘッダスロットへタイトル＋アクションを注入する。
/// パネル埋め込み（note 分割ビュー）では描画しない＝グローバルヘッダに触れない。
/// null を返すコンポーネントなので、レイアウトには一切影響しない。
export function ChatPageHeaderSlot({ title, onShare }: { title: string; onShare: () => void }) {
  usePageHeader(
    () => (
      <PageHeaderBar title={title}>
        <ChatHeaderActions onShare={onShare} />
      </PageHeaderBar>
    ),
    [title, onShare],
  );
  return null;
}
