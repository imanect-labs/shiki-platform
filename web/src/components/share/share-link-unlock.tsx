"use client";

import * as React from "react";
import { KeyRound, Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { redeemShareLink } from "@/lib/storage";

/// 現在の URL から共有リンクトークン（`?lt=`）を読む。Suspense 境界を要さないよう
/// `useSearchParams` ではなく `window.location` から読む（クライアント専用）。
export function unlockTokenFromUrl(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("lt");
}

/// 解錠成功後にアドレスバーから共有リンクトークン（`?lt` / `?unlock`）を除去する（C-4/#369）。
/// トークンが URL に残ると、アドレスバーからのコピー転送でパスワードを知らない相手へ実効的な
/// 閲覧権が漏れる。`history.replaceState` で履歴を汚さずに現在エントリを置換する。
function stripUnlockParams(): void {
  if (typeof window === "undefined") return;
  const url = new URL(window.location.href);
  if (!url.searchParams.has("lt") && !url.searchParams.has("unlock")) return;
  url.searchParams.delete("lt");
  url.searchParams.delete("unlock");
  window.history.replaceState(null, "", `${url.pathname}${url.search}${url.hash}`);
}

/// パスワード付き共有リンクの解錠フォーム（#342）。
///
/// パスワード付きリンクの URL（`?lt=<token>&unlock=1`）で開いてアクセスできなかったときに表示し、
/// パスワードを入力して token を `redeem` する。成功したら `onUnlocked`（アクセス再取得/リロード）を
/// 呼ぶ。失敗理由はサーバ側で秘匿されるため、ここでも一律のメッセージだけ出す（オラクルにしない）。
export function ShareLinkUnlock({
  token,
  onUnlocked,
  autoFocus,
}: {
  /// URL `?lt=` のリンクトークン。
  token: string;
  onUnlocked: () => void;
  autoFocus?: boolean;
}) {
  const [password, setPassword] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!password || busy) return;
    setBusy(true);
    setError(null);
    try {
      await redeemShareLink(token, password);
      // C-4（#369）: 解錠できたらアドレスバーから token を除去してから再取得する。
      stripUnlockParams();
      onUnlocked();
    } catch {
      setError("パスワードが正しくないか、リンクの有効期限が切れています。");
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={submit}
      className="mx-auto flex w-full max-w-sm flex-col gap-3 rounded-2xl border border-border/60 bg-card/60 p-5"
    >
      <div className="flex items-center gap-2 text-sm font-medium">
        <KeyRound className="size-4 text-muted-foreground" aria-hidden />
        パスワードで開く
      </div>
      <p className="text-xs text-muted-foreground">
        このリンクはパスワードで保護されています。共有者から受け取ったパスワードを入力してください。
      </p>
      <Input
        data-testid="link-unlock-password"
        type="password"
        autoComplete="off"
        autoFocus={autoFocus}
        value={password}
        onChange={(e) => setPassword(e.target.value)}
        placeholder="パスワード"
      />
      {error ? (
        <p className="text-xs text-destructive" role="alert">
          {error}
        </p>
      ) : null}
      <Button type="submit" disabled={!password || busy} data-testid="link-unlock-submit">
        {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
        開く
      </Button>
    </form>
  );
}
