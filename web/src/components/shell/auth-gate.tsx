"use client";

import * as React from "react";
import { usePathname, useRouter } from "next/navigation";

import { useMe } from "@/hooks/use-me";
import { consumePostLoginNext } from "@/lib/auth";

/// 認証済みシェルの client 側ゲート。
/// - middleware を通過したが /me が 401（失効/無効セッション）なら /login へ誘導する。
/// - ログイン直後（cookie あり）は、控えておいた戻り先を消費して元ページへ復帰する。
/// 中身は常に描画し、判定確定後に必要なら置換遷移する（保護コンテンツ自体は空状態/
/// スケルトンで安全なため、わずかな表示は許容する）。
export function AuthGate({ children }: { children: React.ReactNode }) {
  const { unauthenticated, loading } = useMe();
  const router = useRouter();
  const pathname = usePathname();

  // 失効セッション → ログイン画面へ（戻り先付き）。
  React.useEffect(() => {
    if (loading || !unauthenticated) return;
    const suffix = pathname && pathname !== "/" ? `?next=${encodeURIComponent(pathname)}` : "";
    router.replace(`/login${suffix}`);
  }, [loading, unauthenticated, pathname, router]);

  // ログイン直後の戻り先復帰（callback は常に "/" に戻すため front で補正する）。
  React.useEffect(() => {
    if (loading || unauthenticated) return;
    const next = consumePostLoginNext();
    if (next && next !== pathname) router.replace(next);
  }, [loading, unauthenticated, pathname, router]);

  return <>{children}</>;
}
