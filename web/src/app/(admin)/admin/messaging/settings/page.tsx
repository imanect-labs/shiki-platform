import type { Metadata } from "next";

import { MessagingSettingsView } from "@/components/admin/messaging-settings-view";

export const metadata: Metadata = { title: "メッセージの設定" };

export default function Page() {
  return <MessagingSettingsView />;
}
