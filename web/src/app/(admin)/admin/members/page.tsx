import type { Metadata } from "next";

import { DirectoryMembersView } from "@/components/admin/directory-live/members";

export const metadata: Metadata = { title: "メンバー" };

export default function MembersPage() {
  return <DirectoryMembersView />;
}
