import * as React from "react";

import { cn } from "@/lib/utils";

/// 共有ダイアログ（ドライブ・データ表・アーティファクト）の DialogContent の形。間隔は中身側で持つ
/// （gap-0）。**高さは固定**し、中身（一覧）だけをスクロールさせる。検索・タブの切り替え・読み込み・
/// 共有相手の増減でダイアログの大きさが伸び縮みしないように（共有相手が数十人でも枠は同じ）。
/// スマホは下からのシートで、見えている高さ（キーボード込み）の 85%。
export const SHARE_DIALOG_CONTENT =
  "max-w-lg gap-0 flex flex-col overflow-hidden sm:h-[min(38rem,calc(100dvh-2rem))] max-sm:h-[calc(var(--visible-height,100dvh)*0.85)]";

/// 共有ダイアログの下端に固定する帯（リンクのコピー等）。DialogContent 自体がスクロールする
/// ので、その余白（PC p-6 / スマホ px-5・セーフエリア）を打ち消して端まで敷き、sticky で残す。
/// sticky はスクロール容器の padding の内側で止まるため、bottom も同じ量だけ負にする
/// （0 だと下に余白分の隙間が空き、帯が本文に食い込む）。
export function ShareDialogFooter({ className, children }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn(
        "sticky -bottom-6 z-10 -mx-6 -mb-6 mt-4 flex shrink-0 items-center justify-between gap-3 border-t border-border bg-card px-6 py-3",
        "max-sm:-bottom-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:-mx-5 max-sm:-mb-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:px-5 max-sm:pb-[max(0.75rem,env(safe-area-inset-bottom))]",
        className,
      )}
    >
      {children}
    </div>
  );
}
