/// チャットのモデル選択（#553）。既定は Auto（= `null`・ピルを出さない）。
///
/// 選択はスレッドごとに sessionStorage に置く（タブを閉じれば Auto に戻る）。ホームの入力欄は
/// `home` スコープで持ち、チャットを始めたら新しいスレッドへ移す。送信時は呼び出し側が
/// `readModelChoice(threadId)` で読んで API に載せる（入力欄と送信経路を疎結合に保つ）。

import * as React from "react";

import { fetchLlmModels, type EffortLevel, type LlmModelsResponse } from "@/lib/admin-ai-api";

export type ModelChoice = { model: string; effort: EffortLevel } | null;

export const HOME_SCOPE = "home";
const KEY = "shiki:model-choice:";
const listeners = new Map<string, Set<() => void>>();

function notify(scope: string) {
  listeners.get(scope)?.forEach((l) => l());
}

export function readModelChoice(scope: string): ModelChoice {
  if (typeof sessionStorage === "undefined") return null;
  try {
    const raw = sessionStorage.getItem(KEY + scope);
    return raw ? (JSON.parse(raw) as ModelChoice) : null;
  } catch {
    return null;
  }
}

export function writeModelChoice(scope: string, choice: ModelChoice): void {
  if (typeof sessionStorage === "undefined") return;
  try {
    if (choice) sessionStorage.setItem(KEY + scope, JSON.stringify(choice));
    else sessionStorage.removeItem(KEY + scope);
  } catch {
    /* 容量超過等は無視（Auto のまま） */
  }
  notify(scope);
}

/// ホームで選んだモデルを新しいスレッドへ移す（ホームは Auto に戻る）。
export function moveModelChoice(from: string, to: string): void {
  const choice = readModelChoice(from);
  if (!choice) return;
  writeModelChoice(to, choice);
  writeModelChoice(from, null);
}

/// スコープのモデル選択を購読する。
export function useModelChoice(scope: string): [ModelChoice, (c: ModelChoice) => void] {
  const subscribe = React.useCallback(
    (cb: () => void) => {
      let set = listeners.get(scope);
      if (!set) listeners.set(scope, (set = new Set()));
      set.add(cb);
      return () => set.delete(cb);
    },
    [scope],
  );
  // sessionStorage の文字列をスナップショットにする（同値なら再描画しない）。
  const raw = React.useSyncExternalStore(
    subscribe,
    () => {
      try {
        return sessionStorage.getItem(KEY + scope);
      } catch {
        return null;
      }
    },
    () => null,
  );
  const choice = React.useMemo<ModelChoice>(() => {
    try {
      return raw ? (JSON.parse(raw) as ModelChoice) : null;
    } catch {
      return null;
    }
  }, [raw]);
  const set = React.useCallback((c: ModelChoice) => writeModelChoice(scope, c), [scope]);
  return [choice, set];
}

let modelsPromise: Promise<LlmModelsResponse | null> | null = null;

/// 選べるモデル（ページ内で 1 回だけ取得して共有する）。取れなければ `null`（選択 UI を出さない）。
export function useLlmModels(): LlmModelsResponse | null {
  const [data, setData] = React.useState<LlmModelsResponse | null>(null);
  React.useEffect(() => {
    let active = true;
    modelsPromise ??= fetchLlmModels().catch(() => {
      modelsPromise = null; // 次のマウントで取り直す
      return null;
    });
    void modelsPromise.then((d) => active && setData(d));
    return () => {
      active = false;
    };
  }, []);
  return data;
}
