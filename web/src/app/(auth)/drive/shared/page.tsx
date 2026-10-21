import type { Metadata } from "next";

import { SharedView } from "@/components/drive/shared-view";
import { DriveChrome } from "@/components/drive/drive-chrome";
import { PageContainer } from "@/components/shell/page-container";

export const metadata: Metadata = { title: "共有済み" };

export default function SharedPage() {
  return (
    <PageContainer title="共有済み" description="自分に共有されたファイルとフォルダです。">
      {/* スマホ: 右上の ⋯ でドライブの区画を切り替える（ドロワには子メニューを出さない）。 */}
      <DriveChrome current="shared" back={{ href: "/drive", label: "ドライブへ戻る" }} />
      <SharedView />
    </PageContainer>
  );
}
