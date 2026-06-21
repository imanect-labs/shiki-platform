"use client";

import { useEffect, useState } from "react";

import { fetchMe, type MeResponse } from "@/lib/api";
import { login, logout } from "@/lib/auth";

export default function Home() {
  const [me, setMe] = useState<MeResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    // StrictMode の二重実行・アンマウント後の setState を防ぐためのガード。
    let active = true;
    fetchMe()
      .then((m) => {
        if (active) setMe(m);
      })
      .catch((e) => {
        if (active) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, []);

  if (loading) return <p>読み込み中…</p>;

  return (
    <main>
      <h1>shiki</h1>
      {me ? (
        <>
          <p>ログイン済み。</p>
          <pre style={{ background: "#f4f4f5", padding: "1rem", borderRadius: 8 }}>
            {JSON.stringify(me, null, 2)}
          </pre>
          <button onClick={() => void logout()}>ログアウト</button>
        </>
      ) : (
        <>
          {error ? <p>未ログインです。</p> : null}
          <button onClick={login}>Keycloak でログイン</button>
        </>
      )}
    </main>
  );
}
