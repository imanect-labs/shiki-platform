import type { Metadata } from "next";

import { TrashView } from "@/components/drive/trash-view";
import { PageContainer } from "@/components/shell/page-container";

export const metadata: Metadata = { title: "ゴミ箱" };

export default function TrashPage() {
  return (
    <PageContainer title="ゴミ箱" description="削除したファイルやフォルダを復元できます。">
      <TrashView />
    </PageContainer>
  );
}
