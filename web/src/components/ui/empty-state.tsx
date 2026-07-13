import * as React from "react";
import { Leaf, type LucideIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { currentSeasonIndex, seasonVar } from "@/lib/season";

type EmptyStateProps = React.ComponentProps<"div"> & {
  icon?: LucideIcon;
  title: string;
  description?: string;
  /// 補助アクション（例: 「アップロード」ボタン）。
  action?: React.ReactNode;
  /// アイコンバッジに今季の葉を小さく添える（ブランド identity を空の瞬間にも運ぶ）。
  seasonal?: boolean;
};

/// データが無いことを丁寧に伝える共通サーフェス（フェイクデータの代わり）。
/// backend 未実装の一覧（最近/お気に入り/ゴミ箱/チャット履歴）でも使う。
function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  seasonal,
  className,
  ...props
}: EmptyStateProps) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border px-6 py-16 text-center",
        className,
      )}
      {...props}
    >
      {Icon ? (
        <div className="relative flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground">
          <Icon className="size-6" aria-hidden />
          {seasonal ? (
            <Leaf
              className="absolute -right-1 -top-1 size-4 rotate-12"
              style={{ color: seasonVar(currentSeasonIndex()) }}
              strokeWidth={2.5}
              aria-hidden
            />
          ) : null}
        </div>
      ) : null}
      <div className="flex flex-col gap-1">
        <p className="text-sm font-semibold text-foreground">{title}</p>
        {description ? (
          <p className="max-w-sm text-sm text-muted-foreground">{description}</p>
        ) : null}
      </div>
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export { EmptyState };
