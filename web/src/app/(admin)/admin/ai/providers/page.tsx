import type { Metadata } from "next";

import { AiProvidersView } from "@/components/admin/ai-providers-view";

export const metadata: Metadata = { title: "AI プロバイダ" };

export default function AiProvidersPage() {
  return <AiProvidersView />;
}
