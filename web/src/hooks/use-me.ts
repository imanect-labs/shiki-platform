"use client";

import * as React from "react";

import { fetchMe, type MeResponse } from "@/lib/api";

export type MeState = {
  data: MeResponse | null;
  /// 未認証（/me が 401）。ログイン導線の表示に使う。
  unauthenticated: boolean;
  error: string | null;
  loading: boolean;
};

/// 現在ユーザーを取得する client フック。
/// 既存 `fetchMe`（BFF プロキシ＋セッション Cookie）を再利用し、StrictMode の
/// 二重実行に耐えるよう `active` ガードを置く。401 は「未認証」として扱う。
export function useMe(): MeState {
  const [state, setState] = React.useState<MeState>({
    data: null,
    unauthenticated: false,
    error: null,
    loading: true,
  });

  React.useEffect(() => {
    let active = true;
    fetchMe()
      .then((data) => {
        if (active) setState({ data, unauthenticated: false, error: null, loading: false });
      })
      .catch((e: unknown) => {
        if (!active) return;
        const message = e instanceof Error ? e.message : String(e);
        const unauthenticated = message.includes("401");
        setState({
          data: null,
          unauthenticated,
          error: unauthenticated ? null : message,
          loading: false,
        });
      });
    return () => {
      active = false;
    };
  }, []);

  return state;
}
