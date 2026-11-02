"use client";

import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import type { OwnedSharedListState } from "@/hooks/use-owned-shared-list";
import { cn } from "@/lib/utils";

/// 一覧の続きの「さらに読み込む」と、読み切れなかったこと・続きの失敗の 1 文（[`useOwnedSharedList`]・BUG-162・
/// BUG-185・BUG-186）。読んでいない続きが無ければ何も出さない。
///
/// - 自分のものの続きがあるだけなら、ボタンだけを出す（ふつうの続き）。小さい形（選択肢の下）では、選択肢に無いものが
///   あることも言う。
/// - 共有された分だけが残っているなら、共有された分を読み切れていないことを言う（`found` は一覧にもう出ているものが
///   あるか。無ければ「まだ見つかっていません」と言い、空の状態に見せない）。
/// - 続きの頼みが失敗したら、読み込めなかったことを言う（押すと、失敗した位置からもう一度読む）。
export function ListMore({
  noun,
  found,
  list,
  compact = false,
}: {
  /// 一覧の名詞（スキル・アプリ・ワークフロー）。
  noun: string;
  found: boolean;
  list: Pick<OwnedSharedListState<unknown>, "hasMore" | "ownedNext" | "failed" | "loadingMore" | "loadMore">;
  /// 欄の下に出す小さい形（選択肢の一覧の下など）。
  compact?: boolean;
}) {
  if (!list.hasMore) return null;
  const [first, second] =
    list.failed === "owned"
      ? [`${noun}の続きを読み込めませんでした。`, "「さらに読み込む」でもう一度試します。"]
      : list.failed === "shared"
        ? [`共有された${noun}の続きを読み込めませんでした。`, "「さらに読み込む」でもう一度試します。"]
        : list.ownedNext
          ? compact
            ? [`ほかにも${noun}があります。`, "「さらに読み込む」で選択肢に足します。"]
            : [null, null]
          : found
            ? [`共有された${noun}の続きは、まだ見つかっていません。`, "「さらに読み込む」で続きを探します。"]
            : [`共有された${noun}は、まだ見つかっていません。`, "「さらに読み込む」で続きを探します。"];
  const failed = list.failed !== null;
  return (
    <div
      className={cn(
        "flex",
        compact ? "flex-wrap items-center gap-x-2 gap-y-1" : "flex-col items-center gap-2 py-6 text-center",
      )}
      data-testid="list-more"
    >
      {first ? (
        // 狭い幅では文の切れ目で折り返す（「さらに読み込む」の途中で割らない）。
        <p
          className={cn(compact ? "text-xs" : "text-sm", failed ? "text-destructive" : "text-muted-foreground")}
          role={failed ? "alert" : undefined}
        >
          <span className="inline-block">{first}</span>
          <span className="inline-block">{second}</span>
        </p>
      ) : null}
      <Button
        type="button"
        variant="outline"
        size="sm"
        className={compact ? "h-7 px-2 text-xs pointer-coarse:h-10" : undefined}
        onClick={list.loadMore}
        disabled={list.loadingMore}
        data-testid="list-more-button"
      >
        {list.loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
        さらに読み込む
      </Button>
    </div>
  );
}

/// 一覧の最初の読み込みに失敗したときの 1 文と「もう一度読み込む」（空の一覧に見せない・付録 C の P5）。
///
/// `what` は読み込めなかったもの（例: スキルの一覧・バージョン履歴）。
export function ListLoadFailed({
  what,
  onRetry,
  compact = false,
  className,
}: {
  what: string;
  onRetry: () => void;
  /// 欄の下に出す小さい形。
  compact?: boolean;
  className?: string;
}) {
  if (compact) {
    return (
      <p
        role="alert"
        className={cn("flex flex-wrap items-center gap-x-2 text-xs text-destructive", className)}
        data-testid="list-load-failed"
      >
        {what}を読み込めませんでした。
        <button type="button" className="text-primary underline underline-offset-2" onClick={onRetry}>
          もう一度読み込む
        </button>
      </p>
    );
  }
  return (
    <div className={cn("flex flex-col items-center gap-3 py-16 text-center", className)} data-testid="list-load-failed">
      <p className="text-sm text-destructive" role="alert">
        {what}を読み込めませんでした。
      </p>
      <Button variant="outline" size="sm" onClick={onRetry}>
        もう一度読み込む
      </Button>
    </div>
  );
}
