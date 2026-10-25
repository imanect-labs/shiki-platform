import type { Metadata } from "next";
import { Suspense } from "react";

import { DataOptimizerView } from "@/components/admin/data-optimizer-view";

export const metadata: Metadata = { title: "クエリ最適化" };

export default function Page() {
  // ?table= を読むので Suspense で包む（静的書き出しで useSearchParams を使う Next の決まり）。
  return (
    <Suspense>
      <DataOptimizerView />
    </Suspense>
  );
}
