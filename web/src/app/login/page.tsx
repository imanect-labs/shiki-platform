"use client";

import * as React from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Loader2, LogIn, ShieldCheck } from "lucide-react";

import { checkSession, login } from "@/lib/auth";
import { Button } from "@/components/ui/button";

/// `?next=` はクエリ由来の未信頼値。オープンリダイレクト / XSS（`javascript:` や
/// `//evil.example.com`・絶対 URL）を防ぐため、同一オリジンの相対パス
/// （`/` 始まり・`//` や `/\` でない）だけを許可し、それ以外は "/" に丸める。
function safeNext(value: string | null): string {
  if (!value || !value.startsWith("/")) return "/";
  if (value.startsWith("//") || value.startsWith("/\\")) return "/";
  return value;
}

/// ログイン画面（シェル外）。
/// - 既にログイン済みなら戻り先（無ければ "/"）へ即リダイレクト。
/// - 未ログインなら Keycloak への導線を出す。`?next=` は OIDC ラウンドトリップを
///   跨いで保持するため login() が sessionStorage に控える。
/// - `?error=` が付いていれば失敗メッセージを表示する。
function LoginInner() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const next = safeNext(searchParams.get("next"));
  const hasError = searchParams.get("error") !== null;

  // checking: 初回のセッション確認中 / authed: 確認済みでリダイレクト中。
  const [status, setStatus] = React.useState<"checking" | "anonymous" | "redirecting">(
    "checking",
  );
  const [submitting, setSubmitting] = React.useState(false);

  React.useEffect(() => {
    let active = true;
    checkSession().then((authenticated) => {
      if (!active) return;
      if (authenticated) {
        setStatus("redirecting");
        router.replace(next);
      } else {
        setStatus("anonymous");
      }
    });
    return () => {
      active = false;
    };
  }, [next, router]);

  const onLogin = () => {
    setSubmitting(true);
    login(next);
  };

  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center text-center">
          <span
            className="bg-clip-text text-[26px] font-bold tracking-[-0.02em] text-transparent"
            style={{
              backgroundImage:
                "linear-gradient(100deg, var(--season-spring), var(--season-summer) 38%, var(--season-autumn) 66%, var(--season-winter))",
            }}
          >
            Shiki
          </span>
          <p className="mt-1.5 text-sm text-muted-foreground">
            権限考慮 AI ワークスペース
          </p>
        </div>

        <div className="rounded-2xl border border-border bg-card p-6 shadow-sm">
          <h1 className="text-center text-lg font-semibold tracking-tight text-foreground">
            ログイン
          </h1>
          <p className="mt-1.5 text-center text-sm text-muted-foreground">
            組織アカウント（Keycloak）で続行します。
          </p>

          {hasError ? (
            <p
              role="alert"
              className="mt-4 rounded-lg bg-destructive/10 px-3 py-2 text-center text-sm text-destructive"
            >
              ログインに失敗しました。もう一度お試しください。
            </p>
          ) : null}

          <div className="mt-6">
            {status === "checking" || status === "redirecting" ? (
              <div className="flex h-9 items-center justify-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" aria-hidden />
                {status === "checking" ? "セッションを確認中…" : "リダイレクト中…"}
              </div>
            ) : (
              <Button className="w-full" onClick={onLogin} disabled={submitting}>
                {submitting ? (
                  <Loader2 className="size-4 animate-spin" aria-hidden />
                ) : (
                  <LogIn className="size-4" aria-hidden />
                )}
                Keycloak でログイン
              </Button>
            )}
          </div>
        </div>

        <p className="mt-5 flex items-center justify-center gap-1.5 text-center text-xs text-muted-foreground">
          <ShieldCheck className="size-3.5" aria-hidden />
          トークンはサーバ側でのみ管理されます。
        </p>
      </div>
    </div>
  );
}

/// useSearchParams は Suspense 境界が必要（静的化時の要件）。
export default function LoginPage() {
  return (
    <React.Suspense fallback={null}>
      <LoginInner />
    </React.Suspense>
  );
}
