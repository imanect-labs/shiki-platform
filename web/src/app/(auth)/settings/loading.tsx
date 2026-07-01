import { PageContainer } from "@/components/shell/page-container";
import { Skeleton } from "@/components/ui/skeleton";

/// 設定遷移中のスケルトン。設定セクションの行を擬似表示して体感遅延を抑える。
export default function SettingsLoading() {
  return (
    <PageContainer title="設定" description="プロフィールや表示設定を管理します。">
      <div aria-busy aria-label="読み込み中" className="flex flex-col gap-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div
            key={i}
            className="flex items-center justify-between gap-4 rounded-xl border border-border p-4"
          >
            <div className="flex flex-col gap-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-60" />
            </div>
            <Skeleton className="h-8 w-20 rounded-md" />
          </div>
        ))}
      </div>
    </PageContainer>
  );
}
