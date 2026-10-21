import type { Metadata } from "next";

import { AiUsageView } from "@/components/admin/ai-usage-view";

export const metadata: Metadata = { title: "AI 利用量" };

export default function AiUsagePage() {
  return <AiUsageView />;
}
