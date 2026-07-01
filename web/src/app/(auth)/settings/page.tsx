import type { Metadata } from "next";
import { Settings2 } from "lucide-react";

import { PageContainer } from "@/components/shell/page-container";
import { EmptyState } from "@/components/ui/empty-state";

export const metadata: Metadata = { title: "設定" };

/// 設定の枠。中身（プロフィール/表示/通知 等）は別 issue（#69 / Task 1.14）。
export default function SettingsPage() {
  return (
    <PageContainer title="設定" description="プロフィールや表示設定を管理します。">
      <EmptyState
        icon={Settings2}
        title="設定画面は準備中です"
        description="プロフィール・表示・通知などの設定を近日追加します。"
      />
    </PageContainer>
  );
}
