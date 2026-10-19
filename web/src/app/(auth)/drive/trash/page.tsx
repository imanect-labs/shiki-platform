import type { Metadata } from "next";

import { TrashView } from "@/components/drive/trash-view";
import { DriveChrome } from "@/components/drive/drive-chrome";
import { PageContainer } from "@/components/shell/page-container";

export const metadata: Metadata = { title: "ゴミ箱" };

export default function TrashPage() {
  return (
    <PageContainer title="ゴミ箱" description="削除したファイルやフォルダを復元できます。">
      {/* スマホ: 右上の ⋯ でドライブの区画を切り替える（ドロワには子メニューを出さない）。 */}
      <DriveChrome current="trash" back={{ href: "/drive", label: "ドライブへ戻る" }} />
      <TrashView />
    </PageContainer>
  );
}
