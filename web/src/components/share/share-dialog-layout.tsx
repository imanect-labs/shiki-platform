import * as React from "react";

import { cn } from "@/lib/utils";

/// 共有ダイアログ（ドライブ・アーティファクト）の DialogContent の形。間隔は中身側で持つ
/// （gap-0）。スマホでは Dialog が下からのシート（max-h 92dvh・内側スクロール）になる。
export const SHARE_DIALOG_CONTENT = "max-w-lg gap-0";

/// 共有ダイアログの下端に固定する帯（リンクのコピー等）。DialogContent 自体がスクロールする
/// ので、その余白（PC p-6 / スマホ px-5・セーフエリア）を打ち消して端まで敷き、sticky で残す。
/// sticky はスクロール容器の padding の内側で止まるため、bottom も同じ量だけ負にする
/// （0 だと下に余白分の隙間が空き、帯が本文に食い込む）。
export function ShareDialogFooter({ className, children }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn(
        "sticky -bottom-6 z-10 -mx-6 -mb-6 mt-4 flex items-center justify-between gap-3 border-t border-border bg-card px-6 py-3",
        "max-sm:-bottom-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:-mx-5 max-sm:-mb-[max(1.25rem,env(safe-area-inset-bottom))] max-sm:px-5 max-sm:pb-[max(0.75rem,env(safe-area-inset-bottom))]",
        className,
      )}
    >
      {children}
    </div>
  );
}
