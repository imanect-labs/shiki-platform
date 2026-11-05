"use client";

/// アプリを使う人自身の権限の確かめ（GAP-37・BUG-217）。
///
/// 開いたバージョンごとに 1 回確かめ、止めたボタンを押したとき・画面に戻ったとき（タブの表示・ウィンドウのフォーカス）
/// に確かめ直す（作成した人に共有してもらった後に、開き直さずに押せる・表が出る）。問い合わせが増えすぎないよう、
///
/// - 問い合わせは同時に 1 つだけ（確かめている間に頼まれたら、同じ答えを待つ）。
/// - 前の確かめから決まった間隔より短ければ、前の答えを使う（押したとき 2 秒・画面に戻ったとき 10 秒）。
/// - 止めているもの（足りない・範囲が狭い部品）が無ければ、画面に戻っても確かめ直さない。
///
/// 判定はサーバー（`GET /mini-apps/{id}/access`）が決める。読めなかったときは止めない（押したときにサーバーが本人の権限で
/// 確かめる）。

import * as React from "react";

import { getMyMiniAppAccess, type MyAppAccess } from "@/lib/artifact-api";

/// 止めたボタンを押したときに、前の確かめを使い回す間隔（二度押しで 2 回問わない）。
export const RECHECK_ON_PRESS_MS = 2_000;
/// 画面に戻ったときに、前の確かめを使い回す間隔（タブを行き来するたびに問わない）。
export const RECHECK_ON_RETURN_MS = 10_000;

type CheckState = {
  /// 確かめているアプリとバージョン。
  key: string | null;
  inflight: Promise<MyAppAccess | null> | null;
  /// 前の確かめが終わった時刻（0 はまだ）。
  at: number;
  latest: MyAppAccess | null;
};

/// 使う人自身に足りない権限（`access`）と、確かめ直す関数（`check`。`minGapMs` より前の確かめが新しければそれを返す・
/// 読めなかったときは null）。
export function useMyAppAccess(
  appId: string | null,
  version: number | null,
): { access: MyAppAccess | null; check: (minGapMs: number) => Promise<MyAppAccess | null> } {
  const [access, setAccess] = React.useState<MyAppAccess | null>(null);
  const key = appId != null && version != null ? `${appId}@${version}` : null;
  const state = React.useRef<CheckState>({ key: null, inflight: null, at: 0, latest: null });

  const check = React.useCallback(
    (minGapMs: number): Promise<MyAppAccess | null> => {
      const s = state.current;
      if (appId == null || version == null || s.key !== key) return Promise.resolve(null);
      if (s.inflight) return s.inflight;
      if (s.at > 0 && Date.now() - s.at < minGapMs) return Promise.resolve(s.latest);
      const forKey = key;
      const p = getMyMiniAppAccess(appId, version)
        .then((a) => {
          if (state.current.key !== forKey) return null;
          state.current.latest = a;
          // 同じ答えなら作り直さない（ボタンと表の部品を描き直さない）。
          setAccess((prev) => (prev && JSON.stringify(prev) === JSON.stringify(a) ? prev : a));
          return a;
        })
        .catch(() => null)
        .finally(() => {
          if (state.current.key !== forKey) return;
          state.current.inflight = null;
          state.current.at = Date.now();
        });
      s.inflight = p;
      return p;
    },
    [appId, version, key],
  );

  // 開いたバージョンごとに 1 回確かめる。
  React.useEffect(() => {
    state.current = { key, inflight: null, at: 0, latest: null };
    setAccess(null);
    if (key) void check(0);
  }, [key, check]);

  // 止めているものがあるときだけ、画面に戻ったら確かめ直す。
  const blocking = access?.parts.some((p) => !p.unavailable) ?? false;
  React.useEffect(() => {
    if (!blocking) return;
    const again = () => void check(RECHECK_ON_RETURN_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") again();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", again);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", again);
    };
  }, [blocking, check]);

  return { access, check };
}
