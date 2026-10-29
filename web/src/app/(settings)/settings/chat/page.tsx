import type { Metadata } from "next";

import { ChatSettingsView } from "@/components/settings/chat-settings-view";

export const metadata: Metadata = { title: "チャット" };

export default function Page() {
  return <ChatSettingsView />;
}
