import type { Metadata } from "next";

import { MockAiView } from "@/components/settings/mock-ai-view";

export const metadata: Metadata = { title: "パーソナライズ" };

export default function Page() {
  return <MockAiView />;
}
