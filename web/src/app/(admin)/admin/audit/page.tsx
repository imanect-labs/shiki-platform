import type { Metadata } from "next";
import { Suspense } from "react";

import { AuditView } from "@/components/admin/audit/audit-view";

export const metadata: Metadata = { title: "監査ログ" };

export default function AuditPage() {
  // 検索条件を URL（useSearchParams）に持つので Suspense で包む。
  return (
    <Suspense>
      <AuditView />
    </Suspense>
  );
}
