"use client";

import * as React from "react";

/// localStorage に同期する永続 state。
/// 初回は SSR/CSR で同一の `initialValue` を返し（hydration mismatch 回避）、
/// mount 後の effect で保存値へ寄せる。別タブの変更にも storage イベントで追従する。
export function useLocalStorage<T>(
  key: string,
  initialValue: T,
): [T, (value: T | ((prev: T) => T)) => void] {
  const [value, setValue] = React.useState<T>(initialValue);

  // mount 後に保存値を読み込む。
  React.useEffect(() => {
    try {
      const raw = window.localStorage.getItem(key);
      if (raw !== null) setValue(JSON.parse(raw) as T);
    } catch {
      // パース不能・アクセス不可なら initialValue のまま続行する。
    }
    // key 変更時のみ再読込。
  }, [key]);

  const set = React.useCallback(
    (next: T | ((prev: T) => T)) => {
      setValue((prev) => {
        const resolved = next instanceof Function ? next(prev) : next;
        try {
          window.localStorage.setItem(key, JSON.stringify(resolved));
        } catch {
          // 保存に失敗しても UI 状態は更新する。
        }
        return resolved;
      });
    },
    [key],
  );

  // 別タブでの変更に追従。
  React.useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key !== key || e.newValue === null) return;
      try {
        setValue(JSON.parse(e.newValue) as T);
      } catch {
        // 無視
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [key]);

  return [value, set];
}
