import type { Metadata } from "next";

import { ConnectionsView } from "@/components/settings/connections-view";

export const metadata: Metadata = { title: "接続済みアプリ" };

export default function Page() {
  return <ConnectionsView />;
}
