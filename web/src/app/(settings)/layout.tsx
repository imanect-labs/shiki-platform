import type { ReactNode } from "react";

import { SettingsShell } from "@/components/settings/settings-shell";
import { AuthGate } from "@/components/shell/auth-gate";

/// ユーザー設定（/settings/*）。管理コンソールと同じ 2 段の並びのシェルで包む。
export default function SettingsLayout({ children }: { children: ReactNode }) {
  return (
    <AuthGate>
      <SettingsShell>{children}</SettingsShell>
    </AuthGate>
  );
}
