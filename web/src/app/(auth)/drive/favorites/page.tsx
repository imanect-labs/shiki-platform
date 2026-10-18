import type { Metadata } from "next";
import { Star } from "lucide-react";

import { DriveChrome } from "@/components/drive/drive-chrome";
import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "お気に入り" };

export default function FavoritesPage() {
  return (
    <PageContainer title="お気に入り" description="星を付けたファイルにすばやくアクセスできます。">
      {/* スマホ: 右上の ⋯ でドライブの区画を切り替える（ドロワには子メニューを出さない）。 */}
      <DriveChrome current="favorites" back={{ href: "/drive", label: "ドライブへ戻る" }} />
      <EmptyState
        icon={Star}
        title="お気に入りはまだありません"
        description="ファイルに星を付けると、ここに集約されます。"
      />
    </PageContainer>
  );
}
