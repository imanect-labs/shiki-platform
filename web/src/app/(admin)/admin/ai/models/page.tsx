import type { Metadata } from "next";

import { AiModelsView } from "@/components/admin/ai-models-view";

export const metadata: Metadata = { title: "AI モデル" };

export default function AiModelsPage() {
  return <AiModelsView />;
}
