import type { Metadata } from "next";

import { MockAuditView } from "@/components/admin/mock-audit-view";

export const metadata: Metadata = { title: "監査ログ" };

export default function MockAuditPage() {
  return <MockAuditView />;
}
