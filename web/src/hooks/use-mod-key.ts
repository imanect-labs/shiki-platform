"use client";

import * as React from "react";

function isApplePlatform(): boolean {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  const platform = nav.userAgentData?.platform ?? nav.platform ?? "";
  return /mac|iphone|ipad|ipod/i.test(platform);
}

const noop = () => () => {};

/// 修飾キーの表示（Mac・iOS は「⌘」、ほかは「Ctrl」）。サーバ描画と初回は「Ctrl」（hydration を揃える）。
export function useModKeyLabel(): "⌘" | "Ctrl" {
  return React.useSyncExternalStore(
    noop,
    () => (isApplePlatform() ? "⌘" : "Ctrl"),
    () => "Ctrl",
  );
}
