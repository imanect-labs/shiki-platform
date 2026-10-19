import type { Metadata } from "next";
import { Clock } from "lucide-react";

import { DriveChrome } from "@/components/drive/drive-chrome";
import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "最近使った" };

export default function RecentPage() {
  return (
    <PageContainer title="最近使った" description="直近に開いたファイルがここに表示されます。">
      {/* スマホ: 右上の ⋯ でドライブの区画を切り替える（ドロワには子メニューを出さない）。 */}
      <DriveChrome current="recent" back={{ href: "/drive", label: "ドライブへ戻る" }} />
      <EmptyState
        icon={Clock}
        title="最近使ったファイルはありません"
        description="ファイルを開くと、ここに履歴が表示されます。"
      />
    </PageContainer>
  );
}
