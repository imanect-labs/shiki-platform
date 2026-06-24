import type { ReactNode } from "react";

import { AppShell } from "@/components/shell/app-shell";
import { AuthGate } from "@/components/shell/auth-gate";

/// 認証済みルート群（/・/c/*・/drive*・/settings）を共通シェルでラップする。
/// ログイン画面はこの route group の外に置くためシェルを継承しない。
/// 一次ゲートは middleware（cookie 有無）、二次ゲートは AuthGate（/me の実効性）。
export default function AuthedLayout({ children }: { children: ReactNode }) {
  return (
    <AuthGate>
      <AppShell>{children}</AppShell>
    </AuthGate>
  );
}
