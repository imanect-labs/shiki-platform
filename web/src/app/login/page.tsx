"use client";

import * as React from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Loader2, LogIn } from "lucide-react";

import { checkSession, claimAutoLogin, login } from "@/lib/auth";
import { Button } from "@/components/ui/button";
import { BrandLogo } from "@/components/shell/brand-logo";

/// `?next=` はクエリ由来の未信頼値。オープンリダイレクト / XSS（`javascript:` や
/// `//evil.example.com`・絶対 URL）を防ぐため、同一オリジンの相対パス
/// （`/` 始まり・`//` や `/\` でない）だけを許可し、それ以外は "/" に丸める。
function safeNext(value: string | null): string {
  if (!value || !value.startsWith("/")) return "/";
  if (value.startsWith("//") || value.startsWith("/\\")) return "/";
  return value;
}

/// ログインの入口（シェル外）。ログイン画面そのものは Keycloak 側の Shiki テーマ
/// （deploy/keycloak/themes/shiki）で、ここは振り分けだけを行う。
/// - 既にログイン済みなら戻り先（無ければ "/"）へ即リダイレクト。
/// - 未ログインならそのまま Keycloak のログイン画面へ送る（ボタンを挟まない）。`?next=` は
///   OIDC ラウンドトリップを跨いで保持するため login() が sessionStorage に控える。
/// - `?error=` が付いている、または短時間に何度自動で送っても未ログインのまま戻ってくる場合は
///   自動では送らず、失敗メッセージと再試行ボタンを出す（ループ防止・claimAutoLogin）。
/// - `?error=account_unusable`（BFF の callback が、ID に使えない文字を含むアカウントを断った・D-205）は、
///   管理者への連絡を促すだけで再試行ボタンを出さない（送り直しても同じ理由で断られるため）。
function LoginInner() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const next = safeNext(searchParams.get("next"));
  const error = searchParams.get("error");
  const hasError = error !== null;
  const accountUnusable = error === "account_unusable";

  const [status, setStatus] = React.useState<"checking" | "redirecting" | "failed">("checking");

  React.useEffect(() => {
    let active = true;
    checkSession().then((authenticated) => {
      if (!active) return;
      if (authenticated) {
        setStatus("redirecting");
        router.replace(next);
      } else if (hasError || !claimAutoLogin()) {
        setStatus("failed");
      } else {
        setStatus("redirecting");
        login(next);
      }
    });
    return () => {
      active = false;
    };
  }, [next, router, hasError]);

  const retry = () => {
    setStatus("redirecting");
    login(next);
  };

  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center text-center">
          <BrandLogo size="lg" />
        </div>

        {status === "failed" ? (
          <div className="rounded-2xl border border-border bg-card p-6 shadow-sm">
            <h1 className="text-center text-lg font-semibold tracking-tight text-foreground">
              ログイン
            </h1>
            <p
              role="alert"
              className="mt-4 rounded-lg bg-destructive/10 px-3 py-2 text-center text-sm text-destructive"
            >
              {accountUnusable ? (
                // 文の途中で折り返さないよう、1 文ずつ行を分ける。
                <>
                  <span className="block">このアカウントは使えません。</span>
                  <span className="block">管理者に連絡してください。</span>
                </>
              ) : (
                "ログインに失敗しました。もう一度お試しください。"
              )}
            </p>
            {accountUnusable ? null : (
              <Button className="mt-6 w-full" onClick={retry}>
                <LogIn className="size-4" aria-hidden />
                もう一度ログイン
              </Button>
            )}
          </div>
        ) : (
          <div
            role="status"
            className="flex h-9 items-center justify-center gap-2 text-sm text-muted-foreground"
          >
            <Loader2 className="size-4 animate-spin" aria-hidden />
            {status === "checking" ? "セッションを確認中…" : "ログイン画面へ移動中…"}
          </div>
        )}
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
