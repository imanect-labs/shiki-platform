import type { Metadata } from "next";

import { InsightsSettingsView } from "@/components/admin/insights/settings-view";

export const metadata: Metadata = { title: "分析の設定" };

export default function Page() {
  return <InsightsSettingsView />;
}
