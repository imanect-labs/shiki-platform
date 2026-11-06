"use client";

/// 会話画面（/c/[id]）のクローム。シェルの `usePageChrome` へ登録して使う。
///
/// - 右上（スマホ）/ バー右端（PC）: 新しいチャット → ⋯（共有・リンクをコピー・会話を書き出し・設定）。
/// - PC のバーには従来どおり文字の「共有」ボタンも出す（幅に余裕があるため）。
/// - スマホではタイトルを出さず、⋯ メニューの先頭に会話名を出す。
///
/// スレッドの名前の変更・ピン留め・削除はサイドバー履歴の行メニューに置く（ここには置かない）。

import { ChartPie, Copy, FileOutput, Settings, Share2, SquarePen } from "lucide-react";
import { useRouter } from "next/navigation";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import type { ContextUsage } from "@/lib/chat-api";
import { contextMenuLabel } from "./context-meter";

async function copyCurrentLink() {
  try {
    await navigator.clipboard.writeText(window.location.href);
    toast({ description: "リンクをコピーしました。" });
  } catch {
    toast({ description: "リンクをコピーできませんでした。" });
  }
}

/// 会話画面（page variant）専用: クロームを登録する。パネル埋め込み（note 分割ビュー）では
/// 描画しない＝グローバルのクロームに触れない。null を返すのでレイアウトには影響しない。
///
/// `contextUsage` があれば ⋯ メニューに「コンテキスト N%」を出す（入力欄に置くとスマホで狭い）。
export function ChatPageHeaderSlot({
  title,
  onShare,
  contextUsage,
  onOpenContext,
  onExport,
}: {
  title: string;
  onShare: () => void;
  contextUsage: ContextUsage | null;
  onOpenContext: () => void;
  /// 会話全体の書き出しダイアログを開く。書き出せる回答が無ければ渡さない（項目を出さない）。
  onExport?: () => void;
}) {
  const contextLabel = contextUsage ? contextMenuLabel(contextUsage) : null;
  const router = useRouter();
  usePageChrome(
    () => ({
      title,
      menuTitle: title,
      flowUnder: true,
      desktopExtra: (
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
      ),
      actions: (
        <ChromeIconButton label="新しいチャット" onClick={() => router.push("/")}>
          <SquarePen aria-hidden />
        </ChromeIconButton>
      ),
      menu: [
        { key: "share", label: "共有", icon: Share2, onSelect: onShare },
        { key: "copy", label: "リンクをコピー", icon: Copy, onSelect: () => void copyCurrentLink() },
        ...(onExport
          ? [{ key: "export", label: "会話を書き出し…", icon: FileOutput, onSelect: onExport, testId: "thread-export" }]
          : []),
        ...(contextLabel
          ? [{ key: "context", label: contextLabel, icon: ChartPie, onSelect: onOpenContext }]
          : []),
        {
          key: "settings",
          label: "設定",
          icon: Settings,
          href: "/settings",
          separatorBefore: contextLabel !== null,
        },
      ],
    }),
    [title, onShare, router, contextLabel, onOpenContext, onExport],
  );
  return null;
}
