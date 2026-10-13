import type { ReactNode } from "react";

import { AdminShell } from "@/components/admin/admin-shell";
import { AuthGate } from "@/components/shell/auth-gate";

/// 管理コンソール（/admin/*）。shiki 本体のシェルとは別の 2 段ナビで包む。
/// 管理者かどうかは AdminShell が /me の is_admin で判定する（API はルートのポリシーで毎回検査）。
export default function AdminLayout({ children }: { children: ReactNode }) {
  return (
    <AuthGate>
      <AdminShell>{children}</AdminShell>
    </AuthGate>
  );
}
