import type { Metadata } from "next";

import { MockNotificationsView } from "@/components/settings/mock-notifications-view";

export const metadata: Metadata = { title: "通知" };

export default function Page() {
  return <MockNotificationsView />;
}
