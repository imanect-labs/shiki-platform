"use client";

import { ArrowUp } from "lucide-react";

import { useMe } from "@/hooks/use-me";
import { Skeleton } from "@/components/ui/skeleton";

/// ホーム＝チャットのプレースホルダ枠（中身は #70 / Phase 3）。
/// ここではシェル統合とローディング/空状態の正本だけを示す。
export default function HomePage() {
  const { data, loading } = useMe();
  const name = data?.email?.split("@")[0] ?? null;

  return (
    <div className="mx-auto flex h-full w-full max-w-3xl flex-col px-4">
      <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
        {loading ? (
          <Skeleton className="h-8 w-64" />
        ) : (
          <h2 className="text-2xl font-semibold tracking-tight">
            {name ? `${name} さん、こんにちは` : "ようこそ"}
          </h2>
        )}
        <p className="max-w-md text-sm text-muted-foreground">
          ナレッジを横断して質問したり、ドライブの資料をもとに対話できます。
          チャット機能は現在準備中です。
        </p>
      </div>

      {/* 入力欄（チャット UI が乗る土台。現時点では無効） */}
      <div className="pb-6">
        <div className="flex items-end gap-2 rounded-2xl border border-border bg-card p-2 shadow-sm">
          <textarea
            rows={1}
            disabled
            aria-label="メッセージ入力（準備中）"
            placeholder="メッセージを送信…（準備中）"
            className="max-h-40 flex-1 resize-none bg-transparent px-3 py-2 text-sm outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
          />
          <button
            type="button"
            disabled
            aria-label="送信"
            className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-primary text-primary-foreground opacity-50"
          >
            <ArrowUp className="size-4" aria-hidden />
          </button>
        </div>
        <p className="mt-2 text-center text-xs text-muted-foreground">
          Shiki は誤った情報を生成することがあります。
        </p>
      </div>
    </div>
  );
}
