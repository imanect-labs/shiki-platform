import type { ReactNode } from "react";

import { AppShell } from "@/components/shell/app-shell";

/// 認証済みルート群（/・/drive*・/settings）を共通シェルでラップする。
/// ログイン画面(#68)はこの route group の外に置くためシェルを継承しない。
export default function AuthedLayout({ children }: { children: ReactNode }) {
  return <AppShell>{children}</AppShell>;
}
