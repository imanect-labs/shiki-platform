import type { Metadata } from "next";

import { DirectoryView } from "@/components/admin/directory-live/view";

export const metadata: Metadata = { title: "ロール" };

export default function RolesPage() {
  return <DirectoryView rolesOnly />;
}
