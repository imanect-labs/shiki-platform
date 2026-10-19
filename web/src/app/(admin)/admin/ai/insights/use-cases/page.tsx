import type { Metadata } from "next";

import { InsightsUseCasesView } from "@/components/admin/insights/usecases-view";

export const metadata: Metadata = { title: "AI の用途" };

export default function Page() {
  return <InsightsUseCasesView />;
}
