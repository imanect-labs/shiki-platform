import type { Metadata } from "next";

import { DataSettingsView } from "@/components/admin/data-settings-view";

export const metadata: Metadata = { title: "データの設定" };

export default function Page() {
  return <DataSettingsView />;
}
