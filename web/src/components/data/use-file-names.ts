"use client";

/// 表・カードのファイルの列（`file_ref`）の値（ドライブのファイルの id）を、ファイル名で見せるために引く。
///
/// 名前は `@/lib/node-name-cache` が 1 件ずつ本人の権限で引き（開けないファイルは「開けない」と覚える）、覚えた
/// 名前は 60 秒で切れる（共有を外された後に名前を出し続けない）。表を開いている間は、切れる前に見えているときだけ
/// 引き直す。描く側は `cachedNodeName` で同期に読む（canvas のセルはフックを使えない）。

import * as React from "react";

import { resolveNodeNames } from "@/lib/node-name-cache";

/// 引き直しを確かめる間隔（引き直すのは期限の近い id だけ）。
const REFRESH_EVERY_MS = 20_000;

/// `ids` の名前を引き、新しく分かったら進む版の番号を返す（描き直しの依存に使う）。
export function useFileNames(ids: readonly string[]): number {
  const key = [...new Set(ids)].sort().join(",");
  const [version, bump] = React.useReducer((n: number) => n + 1, 0);
  React.useEffect(() => {
    if (!key) return;
    const list = key.split(",");
    let active = true;
    const refresh = () => {
      void resolveNodeNames(list).then((done) => {
        if (active && done) bump();
      });
    };
    const onVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    refresh();
    const timer = window.setInterval(onVisible, REFRESH_EVERY_MS);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      active = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [key]);
  return version;
}
