import type { Metadata } from "next";

import { InsightsImpactView } from "@/components/admin/insights/impact-view";

export const metadata: Metadata = { title: "AI の効果" };

export default function Page() {
  return <InsightsImpactView />;
}
