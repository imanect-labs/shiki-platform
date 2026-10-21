import type { Metadata } from "next";

import { InsightsOverviewView } from "@/components/admin/insights/overview-view";

export const metadata: Metadata = { title: "AI 活用分析" };

export default function Page() {
  return <InsightsOverviewView />;
}
