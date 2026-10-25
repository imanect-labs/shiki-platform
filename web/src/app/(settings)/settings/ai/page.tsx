import type { Metadata } from "next";

import { AiView } from "@/components/settings/ai-view";

export const metadata: Metadata = { title: "パーソナライズ" };

export default function Page() {
  return <AiView />;
}
