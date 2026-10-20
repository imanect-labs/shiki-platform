import type { Metadata } from "next";

import { AppearanceView } from "@/components/settings/appearance-view";

export const metadata: Metadata = { title: "表示" };

export default function Page() {
  return <AppearanceView />;
}
