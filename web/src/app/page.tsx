"use client";

import { useEffect, useState } from "react";
import type { User } from "oidc-client-ts";

import { fetchMe, type MeResponse } from "@/lib/api";
import { getUserManager } from "@/lib/auth";

export default function Home() {
  const [user, setUser] = useState<User | null>(null);
  const [me, setMe] = useState<MeResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const mgr = getUserManager();
    mgr
      .getUser()
      .then(async (u) => {
        setUser(u);
        if (u?.access_token) {
          try {
            setMe(await fetchMe(u.access_token));
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }
      })
      .finally(() => setLoading(false));
  }, []);

  const login = () => getUserManager().signinRedirect();
  const logout = () => getUserManager().signoutRedirect();

  if (loading) return <p>読み込み中…</p>;

  return (
    <main>
      <h1>shiki</h1>
      {!user ? (
        <button onClick={login}>Keycloak でログイン</button>
      ) : (
        <>
          <p>ログイン済み。</p>
          {me ? (
            <pre
              style={{ background: "#f4f4f5", padding: "1rem", borderRadius: 8 }}
            >
              {JSON.stringify(me, null, 2)}
            </pre>
          ) : error ? (
            <p style={{ color: "crimson" }}>エラー: {error}</p>
          ) : (
            <p>/me を取得中…</p>
          )}
          <button onClick={logout}>ログアウト</button>
        </>
      )}
    </main>
  );
}
