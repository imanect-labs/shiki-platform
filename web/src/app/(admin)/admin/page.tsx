import type { Metadata } from "next";

import { AdminHomeView } from "@/components/admin/admin-home-view";

export const metadata: Metadata = { title: "管理コンソール" };

export default function AdminHomePage() {
  return <AdminHomeView />;
}
