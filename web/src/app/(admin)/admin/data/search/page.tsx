import type { Metadata } from "next";

import { SearchIngestView } from "@/components/admin/search-ingest-view";

export const metadata: Metadata = { title: "社内文書の検索" };

export default function Page() {
  return <SearchIngestView />;
}
