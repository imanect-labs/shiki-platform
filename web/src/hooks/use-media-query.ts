"use client";

import * as React from "react";

/// SSR 安全な matchMedia フック。初回サーバ描画では `false` を返し、
/// mount 後に実際の値へ同期する（hydration mismatch を避ける）。
export function useMediaQuery(query: string): boolean {
  const subscribe = React.useCallback(
    (callback: () => void) => {
      if (typeof window === "undefined") return () => {};
      const mql = window.matchMedia(query);
      mql.addEventListener("change", callback);
      return () => mql.removeEventListener("change", callback);
    },
    [query],
  );

  const getSnapshot = () =>
    typeof window !== "undefined" && window.matchMedia(query).matches;
  const getServerSnapshot = () => false;

  return React.useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
