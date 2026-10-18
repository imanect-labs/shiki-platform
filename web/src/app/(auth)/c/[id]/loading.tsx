import { Skeleton } from "@/components/ui/skeleton";

/// 会話画面 `/c/[id]` への遷移中スケルトン。チャット送信直後の遷移で会話本文と
/// 入力欄の骨組みを先に見せ、空白の体感遅延を抑える。
export default function ConversationLoading() {
  return (
    <div className="flex h-full flex-col" aria-busy aria-label="読み込み中">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-8 max-md:pt-[calc(4.5rem+env(safe-area-inset-top))]">
          {/* ユーザー発話（右）→ アシスタント（左）の交互骨組み */}
          <div className="flex justify-end">
            <Skeleton className="h-10 w-1/2 rounded-2xl" />
          </div>
          <div className="flex flex-col gap-2">
            <Skeleton className="h-4 w-11/12" />
            <Skeleton className="h-4 w-4/5" />
            <Skeleton className="h-4 w-2/3" />
          </div>
          <div className="flex justify-end">
            <Skeleton className="h-10 w-2/5 rounded-2xl" />
          </div>
          <div className="flex flex-col gap-2">
            <Skeleton className="h-4 w-10/12" />
            <Skeleton className="h-4 w-3/5" />
          </div>
        </div>
      </div>

      <div className="bg-background">
        <div className="mx-auto w-full max-w-3xl px-4 pt-2 pb-[max(0.75rem,env(safe-area-inset-bottom))] md:pb-4 md:pt-3">
          {/* スマホは 1 行のバー、PC は 2 段（実物の高さに合わせて跳ねないように）。 */}
          <Skeleton className="h-[54px] w-full rounded-[26px] md:h-[88px]" />
        </div>
      </div>
    </div>
  );
}
