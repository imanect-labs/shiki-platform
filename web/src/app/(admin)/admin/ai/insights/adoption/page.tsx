import type { Metadata } from "next";

import { InsightsAdoptionView } from "@/components/admin/insights/adoption-view";

export const metadata: Metadata = { title: "利用者と部署" };

export default function Page() {
  return <InsightsAdoptionView />;
}
