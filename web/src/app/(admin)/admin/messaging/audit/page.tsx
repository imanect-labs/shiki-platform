import type { Metadata } from "next";

import { MessagingAuditView } from "@/components/admin/messaging-audit-view";

export const metadata: Metadata = { title: "メッセージの閲覧" };

export default function Page() {
  return <MessagingAuditView />;
}
