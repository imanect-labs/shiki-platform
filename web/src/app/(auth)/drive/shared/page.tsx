import type { Metadata } from "next";
import { Share2 } from "lucide-react";

import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "共有済み" };

/// 共有一覧の枠。backend（GET /shares/shared-with-me）は存在するが、
/// 一覧 UI は Drive 本体（#20）の責務のため、ここでは枠＋空状態に留める。
export default function SharedPage() {
  return (
    <PageContainer title="共有済み" description="自分に共有されたファイルとフォルダです。">
      <EmptyState
        icon={Share2}
        title="共有されたアイテムはありません"
        description="他のユーザーから共有されると、ここに表示されます。"
      />
    </PageContainer>
  );
}
