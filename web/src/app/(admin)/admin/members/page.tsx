import type { Metadata } from "next";

import { MockMembersView } from "@/components/admin/mock-members-view";

export const metadata: Metadata = { title: "メンバー" };

export default function MockMembersPage() {
  return <MockMembersView />;
}
