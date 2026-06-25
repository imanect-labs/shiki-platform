import { PageContainer } from "@/components/shell/page-container";
import { Skeleton } from "@/components/ui/skeleton";

/// ドライブ遷移中のスケルトン。ルートセグメントの Suspense 境界として App Router が
/// 自動表示し、遷移直後の空白（体感遅延）を埋める。
export default function DriveLoading() {
  return (
    <PageContainer title="ドライブ" description="ファイルとフォルダを管理します。">
      <div
        aria-busy
        aria-label="読み込み中"
        className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4"
      >
        {Array.from({ length: 8 }).map((_, i) => (
          <div
            key={i}
            className="flex flex-col gap-3 rounded-xl border border-border p-4"
          >
            <Skeleton className="size-9 rounded-lg" />
            <Skeleton className="h-4 w-3/4" />
            <Skeleton className="h-3 w-1/2" />
          </div>
        ))}
      </div>
    </PageContainer>
  );
}
