import * as React from "react";
import type { Metadata } from "next";

import { SecurityView } from "@/components/settings/security-view";

export const metadata: Metadata = { title: "ログイン方法" };

/// useSearchParams（Keycloak から戻った結果の表示）は Suspense 境界が必要。
export default function Page() {
  return (
    <React.Suspense fallback={null}>
      <SecurityView />
    </React.Suspense>
  );
}
