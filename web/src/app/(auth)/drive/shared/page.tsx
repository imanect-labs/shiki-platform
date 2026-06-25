import type { Metadata } from "next";

import { SharedView } from "@/components/drive/shared-view";
import { PageContainer } from "@/components/shell/page-container";

export const metadata: Metadata = { title: "共有済み" };

export default function SharedPage() {
  return (
    <PageContainer title="共有済み" description="自分に共有されたファイルとフォルダです。">
      <SharedView />
    </PageContainer>
  );
}
