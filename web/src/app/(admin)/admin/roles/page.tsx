import type { Metadata } from "next";

import { MockRolesView } from "@/components/admin/mock-roles-view";

export const metadata: Metadata = { title: "ロール" };

export default function MockRolesPage() {
  return <MockRolesView />;
}
