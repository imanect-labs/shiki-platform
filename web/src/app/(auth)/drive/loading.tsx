import { PageContainer } from "@/components/shell/page-container";
import { Skeleton } from "@/components/ui/skeleton";

/// ドライブ遷移中のスケルトン。ルートセグメントの Suspense 境界として App Router が
/// 自動表示し、遷移直後の空白（体感遅延）を埋める。実際の表示（既定は一覧）に形を揃え、
/// 読み込み後に段組みが入れ替わって跳ねないようにする（スマホ=見出し＋2 行の行、PC=ツールバー＋行）。
export default function DriveLoading() {
  return (
    <PageContainer className="max-md:pb-28 max-md:pt-1">
      <div aria-busy aria-label="読み込み中" className="flex flex-col gap-4 max-md:gap-3">
        {/* スマホの見出し（MobilePageTitle と同じ高さ） */}
        <div className="flex items-end justify-between gap-2 pt-1 md:hidden">
          <div className="flex flex-col gap-2">
            <Skeleton className="h-3 w-28" />
            <Skeleton className="h-7 w-32" />
          </div>
          <Skeleton className="h-9 w-36 rounded-lg" />
        </div>
        {/* PC のツールバー */}
        <div className="flex items-center gap-2.5 max-md:hidden">
          <Skeleton className="size-9 rounded-full" />
          <Skeleton className="h-9 w-28 rounded-lg" />
          <Skeleton className="h-9 w-28 rounded-lg" />
          <Skeleton className="ml-auto h-9 w-72 rounded-lg" />
        </div>
        <div className="flex flex-col max-md:-mx-3">
          {Array.from({ length: 8 }).map((_, i) => (
            <div key={i} className="shiki-dash-bottom flex items-center gap-3.5 px-3 py-2.5 max-md:min-h-[68px]">
              <Skeleton className="size-7 shrink-0 rounded-md max-md:size-8" />
              <div className="flex min-w-0 flex-1 flex-col gap-1.5">
                <Skeleton className="h-4" style={{ width: `${40 + ((i * 17) % 35)}%` }} />
                <Skeleton className="h-3 w-1/3 md:hidden" />
              </div>
              <Skeleton className="hidden h-3 w-32 md:block" />
              <Skeleton className="hidden h-3 w-16 md:block" />
            </div>
          ))}
        </div>
      </div>
    </PageContainer>
  );
}
