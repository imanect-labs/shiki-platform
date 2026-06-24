import type { Metadata } from "next";
import { FolderOpen } from "lucide-react";

import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "ドライブ" };

/// ドライブのホーム枠。ファイルブラウザ本体は別 issue（#20 / Task 1.10）。
/// 本シェルでは現在地と土台のみを提供する。
export default function DriveHomePage() {
  return (
    <PageContainer title="ドライブ" description="ファイルとフォルダを管理します。">
      <EmptyState
        icon={FolderOpen}
        title="ファイルブラウザは準備中です"
        description="アップロード・閲覧・共有に対応したドライブ画面を近日追加します。"
      />
    </PageContainer>
  );
}
