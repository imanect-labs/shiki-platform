import type { Metadata } from "next";
import { Trash2 } from "lucide-react";

import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "ゴミ箱" };

export default function TrashPage() {
  return (
    <PageContainer title="ゴミ箱" description="削除したファイルは一定期間ここに保管されます。">
      <EmptyState
        icon={Trash2}
        title="ゴミ箱は空です"
        description="削除したファイルがここに表示され、復元できます。"
      />
    </PageContainer>
  );
}
