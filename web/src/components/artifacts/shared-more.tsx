"use client";

import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";

/// 共有されたスキル・アプリを読み切れなかったときの 1 文と「さらに読み込む」（BUG-162・[`useArtifactList`]）。
///
/// `found` は一覧にもう出ているものがあるか（無ければ「まだ見つかっていません」と言い、空の状態に見せない）。
export function SharedMore({
  noun,
  found,
  failed,
  loading,
  onMore,
}: {
  /// 一覧の名詞（スキル・アプリ）。
  noun: string;
  found: boolean;
  failed: boolean;
  loading: boolean;
  onMore: () => void;
}) {
  const message = failed
    ? `共有された${noun}の続きを読み込めませんでした。「さらに読み込む」でもう一度試します。`
    : found
      ? `共有された${noun}の続きは、まだ見つかっていません。「さらに読み込む」で続きを探します。`
      : `共有された${noun}は、まだ見つかっていません。「さらに読み込む」で続きを探します。`;
  return (
    <div className="flex flex-col items-center gap-2 py-6 text-center" data-testid="artifacts-shared-more">
      <p className={failed ? "text-sm text-destructive" : "text-sm text-muted-foreground"} role={failed ? "alert" : undefined}>
        {message}
      </p>
      <Button variant="outline" size="sm" onClick={onMore} disabled={loading} data-testid="artifacts-shared-more-button">
        {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
        さらに読み込む
      </Button>
    </div>
  );
}

/// 一覧の最初の読み込みに失敗したときの 1 文と「もう一度読み込む」（空の一覧に見せない・付録 C の P5）。
export function ListLoadFailed({ noun, onRetry }: { noun: string; onRetry: () => void }) {
  return (
    <div className="flex flex-col items-center gap-3 py-16 text-center" data-testid="artifacts-load-failed">
      <p className="text-sm text-destructive" role="alert">
        {noun}の一覧を読み込めませんでした。
      </p>
      <Button variant="outline" size="sm" onClick={onRetry}>
        もう一度読み込む
      </Button>
    </div>
  );
}
