"use client";

import * as React from "react";
import { KeyRound, Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { redeemGeneralAccess } from "@/lib/storage";

/// パスワード付き一般アクセスの解錠フォーム（#338）。
///
/// リソースを開けなかったときに表示し、パスワードを入力して `redeem` する。成功したら
/// `onUnlocked`（アクセス再取得/リロード）を呼ぶ。失敗理由はサーバ側で秘匿されるため、
/// ここでも一律のメッセージだけ出す（オラクルにしない）。
export function GeneralAccessUnlock({
  nodeId,
  onUnlocked,
  autoFocus,
}: {
  nodeId: string;
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
      await redeemGeneralAccess(nodeId, password);
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
        data-testid="ga-unlock-password"
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
      <Button type="submit" disabled={!password || busy} data-testid="ga-unlock-submit">
        {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
        開く
      </Button>
    </form>
  );
}
