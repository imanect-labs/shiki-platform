import type { Metadata } from "next";

import { MockConnectionsView } from "@/components/settings/mock-connections-view";

export const metadata: Metadata = { title: "接続済みアプリ" };

export default function Page() {
  return <MockConnectionsView />;
}
