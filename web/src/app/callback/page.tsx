"use client";

import { useEffect, useState } from "react";

import { getUserManager } from "@/lib/auth";

/// Keycloak からのリダイレクトを受け取り、認可コードを処理してトップへ戻す。
export default function Callback() {
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getUserManager()
      .signinRedirectCallback()
      .then(() => {
        window.location.replace("/");
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  return <p>{error ? `ログインに失敗しました: ${error}` : "ログイン処理中…"}</p>;
}
