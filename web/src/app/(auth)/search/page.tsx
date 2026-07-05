import type { Metadata } from "next";
import { Suspense } from "react";

import { SearchView } from "@/components/search/search-view";
import { PageContainer } from "@/components/shell/page-container";
import { Skeleton } from "@/components/ui/skeleton";

export const metadata: Metadata = { title: "文書検索" };

/// permission-aware 文書検索（FR-3 / Task 2.10）。
/// 自分が読める文書だけから、引用チャンク付きで検索結果を返す。
/// `useSearchParams`（クエリ共有 URL）を含むため Suspense 境界で包む。
export default function SearchPage() {
  return (
    <PageContainer
      title="文書検索"
      description="あなたが閲覧できる文書だけを対象に、意味検索とキーワード検索を組み合わせて探します。"
    >
      <Suspense fallback={<Skeleton className="h-32 w-full" />}>
        <SearchView />
      </Suspense>
    </PageContainer>
  );
}
