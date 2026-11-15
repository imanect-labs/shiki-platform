import type { Metadata } from "next";

import { AgentProfileSettingsView } from "@/components/settings/agent-profile-view";

export const metadata: Metadata = { title: "メッセージのエージェント" };

export default function Page() {
  return <AgentProfileSettingsView />;
}
